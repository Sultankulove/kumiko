//! E9: the Language service against the fake language server of `tests/fixtures`, started
//! through `lsp/resolveLaunch` of a plugin (`kumiko.echo` contributes the server). The server
//! speaks UTF-16, so every position here crosses the encoding boundary (ADR-012).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use kumiko_headless::kumiko_core::language::{CompletionKind, ServerStatus, Severity};
use kumiko_headless::kumiko_core::plugin_host::PluginStore;
use kumiko_headless::{Headless, eventually};
use kumiko_types::{DocumentId, Position, Range, TextEdit, Uri};

use crate::fixtures;

fn binary(package: &'static str) -> PathBuf {
    fixtures::build(package, &[], package, "")
}

fn binaries() -> &'static (PathBuf, PathBuf) {
    static BINARIES: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    BINARIES.get_or_init(|| (binary("kumiko-echo"), binary("kumiko-fake-lsp")))
}

/// `kumiko.echo` with the fake server installed next to its binary.
fn plugin_source(dir: &Path) -> PathBuf {
    let (echo, server) = binaries();
    let manifest = fixtures::workspace_root().join("sdk/examples/echo/plugin.toml");
    let suffix = std::env::consts::EXE_SUFFIX;
    let echo_name = format!("bin/kumiko-echo{suffix}");
    let server_name = format!("bin/kumiko-fake-lsp{suffix}");
    let files = [
        (echo_name.as_str(), echo.as_path()),
        (server_name.as_str(), server.as_path()),
    ];
    fixtures::plugin_source(dir, "echo-lsp-src", &manifest, |m| m, &files)
}

const SOURCE: &str = "hello TODO world\nпривет \nx.al\nhello again\n";

struct Fixture {
    ide: Headless,
    uri: Uri,
    doc: DocumentId,
}

impl Fixture {
    async fn start() -> Self {
        Self::start_with("src/a.fake", SOURCE).await
    }

    async fn start_with(file: &str, source: &str) -> Self {
        let ide = Headless::start_with(|options| {
            let source = plugin_source(options.dirs.data_dir());
            PluginStore::new(&options.dirs).install(&source).unwrap();
        })
        .await
        .unwrap();
        let workspace = ide.workspace().to_path_buf();
        ide.core.open_folder(&workspace).await.unwrap();
        let path = ide.write(file, source).unwrap();
        let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
        let doc = ide.core.documents().open(&uri, None).await.unwrap().id;
        assert_eq!(ide.core.language_of(doc).as_deref(), Some("fakelang"));
        // The service learns about the document from events; the server starts on that.
        let language = ide.core.language();
        eventually("the server running", || {
            language
                .servers()
                .iter()
                .any(|(_, status)| *status == ServerStatus::Running)
        })
        .await
        .unwrap();
        Self { ide, uri, doc }
    }

    fn text(&self) -> String {
        self.ide
            .core
            .documents()
            .snapshot(self.doc)
            .unwrap()
            .text
            .to_string()
    }

    fn edit(&self, at: Position, text: &str) {
        let documents = self.ide.core.documents();
        let version = documents.snapshot(self.doc).unwrap().version;
        documents
            .apply(
                self.doc,
                version,
                vec![TextEdit {
                    range: Range::new(at, at),
                    new_text: text.into(),
                }],
            )
            .unwrap();
    }

    async fn diagnostics_are(&self, count: usize) {
        let language = self.ide.core.language();
        let reached = eventually(&format!("{count} diagnostics"), || {
            language.diagnostics(&self.uri).len() == count
        })
        .await;
        if let Err(error) = reached {
            self.dump();
            panic!("{error}");
        }
    }

    /// What the servers said, for a failing test.
    fn dump(&self) {
        let core = &self.ide.core;
        eprintln!("servers: {:?}", core.language().servers());
        eprintln!("diagnostics: {:?}", core.language().diagnostics(&self.uri));
        for line in core
            .output()
            .lines(kumiko_headless::kumiko_core::LANGUAGE_CHANNEL)
        {
            eprintln!("  {line}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostics_follow_edits_through_the_encoding() {
    let f = Fixture::start().await;
    f.diagnostics_are(1).await;
    let diagnostics = f.ide.core.language().diagnostics(&f.uri);
    assert_eq!(diagnostics[0].severity, Severity::Warning);
    assert_eq!(diagnostics[0].code.as_deref(), Some("todo"));
    assert_eq!(
        diagnostics[0].range,
        Range::new(Position::new(0, 6), Position::new(0, 10))
    );

    // After Cyrillic text: 7 UTF-16 units on the server's side, 13 bytes on ours.
    f.edit(Position::new(1, 13), "TODO");
    f.diagnostics_are(2).await;
    let second = &f.ide.core.language().diagnostics(&f.uri)[1];
    assert_eq!(
        second.range,
        Range::new(Position::new(1, 13), Position::new(1, 17))
    );
    assert_eq!(f.ide.core.language().all_diagnostics().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_hover_and_definition() {
    let f = Fixture::start().await;
    let language = f.ide.core.language();

    let items = language.completion(&f.uri, Position::new(2, 4)).await;
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["alpha", "beta"]);
    assert_eq!(items[0].kind, CompletionKind::Variable);
    let edit = items[0].edit.clone().unwrap();
    assert_eq!(
        edit.range,
        Range::new(Position::new(2, 2), Position::new(2, 4))
    );
    // Resolving fills in the import the item needs.
    let beta = language.resolve_completion(&f.uri, items[1].clone()).await;
    assert_eq!(beta.additional_edits.len(), 1);
    assert_eq!(beta.additional_edits[0].new_text, "import beta;\n");

    let hover = language.hover(&f.uri, Position::new(0, 1)).await.unwrap();
    assert_eq!(hover.contents, "**hello**");

    let found = language.definition(&f.uri, Position::new(3, 2)).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].uri, f.uri);
    assert_eq!(
        found[0].range,
        Range::new(Position::new(0, 0), Position::new(0, 5))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_actions_apply_edits_and_commands() {
    let f = Fixture::start().await;
    f.diagnostics_are(1).await;
    let language = f.ide.core.language();
    let todo = Range::new(Position::new(0, 6), Position::new(0, 10));
    let actions = language.code_actions(&f.uri, todo).await;
    let titles: Vec<&str> = actions.iter().map(|a| a.title.as_str()).collect();
    assert_eq!(titles, ["Replace TODO with DONE", "Insert header"]);
    assert!(actions[0].preferred);

    // The fix comes with codeAction/resolve.
    language.apply_code_action(&actions[0]).await.unwrap();
    assert!(f.text().starts_with("hello DONE world\n"), "{}", f.text());
    f.diagnostics_are(0).await;

    // The command makes the server send workspace/applyEdit.
    language.apply_code_action(&actions[1]).await.unwrap();
    assert!(
        f.text().starts_with("// header\nhello DONE"),
        "{}",
        f.text()
    );
    let snapshot = f.ide.core.documents().snapshot(f.doc).unwrap();
    assert!(snapshot.dirty, "an open document keeps the edit unsaved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crashed_server_is_restarted_with_its_documents() {
    let f = Fixture::start().await;
    f.diagnostics_are(1).await;
    let language = f.ide.core.language();
    let key = language.servers()[0].0.clone();
    let first = language.server_pid(&key).unwrap();

    // The fake server exits on an edit that leaves CRASH in the text; the restarted one gets
    // the document again with didOpen, then the edit that takes CRASH out.
    f.edit(Position::new(3, 0), "CRASH");
    eventually("the server restarted", || {
        language.server_pid(&key).is_some_and(|pid| pid != first)
    })
    .await
    .unwrap();
    let version = f.ide.core.documents().snapshot(f.doc).unwrap().version;
    f.ide
        .core
        .documents()
        .apply(
            f.doc,
            version,
            vec![TextEdit {
                range: Range::new(Position::new(3, 0), Position::new(3, 5)),
                new_text: String::new(),
            }],
        )
        .unwrap();
    eventually("running again", || {
        language.servers()[0].1 == ServerStatus::Running && language.diagnostics(&f.uri).len() == 1
    })
    .await
    .unwrap();
    // The restarted server got the document again and answers about it.
    let hover = language.hover(&f.uri, Position::new(0, 1)).await.unwrap();
    assert_eq!(hover.contents, "**hello**");
    f.ide.core.shutdown().await;
}

const SHOP: &str = "class Shop\n  def total\n    count = 1   \n    count\nx = beta(1, \n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn navigation_and_refactoring() {
    let f = Fixture::start_with("src/shop.fake", SHOP).await;
    let language = f.ide.core.language();

    let symbols = language.symbols(&f.uri).await;
    let outline: Vec<(&str, &str, usize)> = symbols
        .iter()
        .map(|s| (s.name.as_str(), s.kind, s.depth))
        .collect();
    assert_eq!(outline, [("Shop", "class", 0), ("total", "method", 1)]);

    let count = Position::new(2, 5);
    let usages = language.references(&f.uri, count).await;
    let lines: Vec<u32> = usages.iter().map(|l| l.range.start.line).collect();
    assert_eq!(lines, [2, 3]);
    let highlights = language.document_highlights(&f.uri, count).await;
    assert_eq!(highlights.len(), 2);
    assert!(highlights[0].write && !highlights[1].write);

    let help = language
        .signature_help(&f.uri, Position::new(4, 12))
        .await
        .unwrap();
    assert_eq!(help.signatures, ["beta(int a, String b)"]);
    assert_eq!(
        help.active_parameter,
        Some(12..20),
        "the second, after one comma"
    );

    assert_eq!(language.rename(&f.uri, count, "amount").await, Ok(1));
    assert_eq!(f.text().matches("amount").count(), 2, "{}", f.text());
    language.format(&f.uri, 4, true).await.unwrap();
    assert!(f.text().contains("    amount = 1\n"), "{:?}", f.text());
}

/// The plugin talks to its server (`lsp/sendRequest`), and answers what the server asks the
/// client when it declared the method in `lspMiddleware`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_plugin_sends_requests_and_intercepts_server_messages() {
    let f = Fixture::start().await;
    let id = kumiko_types::CommandId::new("echo.lsp").unwrap();
    let args = vec![
        serde_json::json!("fake"),
        serde_json::json!("workspace/executeCommand"),
        serde_json::json!({"command": "fake.ask", "arguments": []}),
    ];
    let answer = f.ide.core.commands().execute(&id, args).await.unwrap();
    assert_eq!(
        answer,
        serde_json::json!({"asked": {"answer": 42, "server": "fake"}})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_progress_is_reported() {
    let f = Fixture::start().await;
    let language = f.ide.core.language();
    eventually("progress", || {
        language.progress().iter().any(|p| {
            p.server == "fake"
                && p.title == "Indexing"
                && p.message.as_deref() == Some("files")
                && p.percentage == Some(50)
        })
    })
    .await
    .unwrap();
}
