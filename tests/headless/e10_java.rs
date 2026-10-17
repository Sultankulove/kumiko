//! E10: the Java plugin (`kumiko.java`) on a small Maven project. Without JDT LS the project
//! still works (highlighting from the grammar package, the core alive, the server stopped
//! with a reason). With JDT LS (`KUMIKO_JDTLS` naming an install, run with `--ignored`):
//! completion, diagnostics, Go to Declaration and the "import" quick fix (MVP criteria 7–9,
//! 18).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use kumiko_headless::Headless;
use kumiko_headless::kumiko_core::language::ServerStatus;
use kumiko_headless::kumiko_core::plugin_host::PluginStore;
use kumiko_types::{DocumentId, Position, Range, Uri};

use crate::fixtures;

fn plugin_source(dir: &Path) -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    let binary = BINARY.get_or_init(|| fixtures::build("kumiko-java", &[], "kumiko-java", ""));
    let root = fixtures::workspace_root().join("plugins/java");
    let entry = format!("bin/kumiko-java{}", std::env::consts::EXE_SUFFIX);
    let schema = root.join("schema/config.json");
    let files = [
        (entry.as_str(), binary.as_path()),
        ("schema/config.json", schema.as_path()),
    ];
    fixtures::plugin_source(dir, "java-src", &root.join("plugin.toml"), |m| m, &files)
}

const POM: &str = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>demo</groupId>
  <artifactId>demo</artifactId>
  <version>1.0</version>
  <properties>
    <maven.compiler.source>17</maven.compiler.source>
    <maven.compiler.target>17</maven.compiler.target>
  </properties>
</project>
"#;

const APP: &str = "package demo;\n\npublic class App {\n    public static void main(String[] args) {\n        List<String> names = null;\n        greet();\n        System.out.\n    }\n\n    static void greet() {\n    }\n}\n";

struct Project {
    ide: Headless,
    uri: Uri,
    doc: DocumentId,
}

/// The Maven project with `kumiko.java` installed; `settings` go to the user settings file.
async fn open(settings: &str) -> Project {
    let settings = settings.to_owned();
    let ide = Headless::start_with(move |options| {
        let source = plugin_source(options.dirs.data_dir());
        PluginStore::new(&options.dirs).install(&source).unwrap();
        let file = options.dirs.user_settings_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, settings).unwrap();
    })
    .await
    .unwrap();
    ide.write("pom.xml", POM).unwrap();
    let path = ide.write("src/main/java/demo/App.java", APP).unwrap();
    let workspace = ide.workspace().to_path_buf();
    ide.core.open_folder(&workspace).await.unwrap();
    let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
    let doc = ide.core.documents().open(&uri, None).await.unwrap().id;
    Project { ide, uri, doc }
}

/// Polls `condition` up to `limit` (JDT LS takes its time to import a project).
async fn until(what: &str, limit: Duration, mut condition: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + limit;
    while !condition().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_jdt_ls_java_still_works() {
    let missing = std::env::temp_dir().join("kumiko-no-jdtls-here");
    let settings = format!(
        "[java.jdtls]\npath = {:?}\n",
        missing.to_string_lossy().replace('\\', "/")
    );
    let p = open(&settings).await;
    assert_eq!(p.ide.core.language_of(p.doc).as_deref(), Some("java"));
    let highlights = p.ide.core.highlights(p.doc, 0..APP.len());
    assert!(highlights.iter().any(|h| h.capture.starts_with("keyword")));

    let language = p.ide.core.language();
    until(
        "the server to give up",
        Duration::from_secs(20),
        async || {
            matches!(
                language.servers().first(),
                Some((_, ServerStatus::Stopped(_)))
            )
        },
    )
    .await;
    let (key, status) = language.servers().remove(0);
    assert_eq!(key.id, "jdtls");
    let ServerStatus::Stopped(reason) = status else {
        unreachable!()
    };
    // Either no JDK 21 on this machine or the missing server; both say what to set.
    assert!(
        reason.contains("java.jdtls.path") || reason.contains("java.home"),
        "{reason}"
    );
    // Requests answer empty instead of failing, and the core lives on.
    assert!(
        language
            .completion(&p.uri, Position::new(6, 19))
            .await
            .is_empty()
    );
    let jdks = p
        .ide
        .core
        .commands()
        .execute(
            &kumiko_types::CommandId::new("java.showJdks").unwrap(),
            vec![],
        )
        .await
        .unwrap();
    assert!(jdks.is_array());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs JDT LS: set KUMIKO_JDTLS to an install folder"]
async fn with_jdt_ls_completion_diagnostics_definition_and_quick_fix() {
    let install = std::env::var("KUMIKO_JDTLS").expect("KUMIKO_JDTLS");
    let settings = format!("[java.jdtls]\npath = {:?}\n", install.replace('\\', "/"));
    let p = open(&settings).await;
    let language = p.ide.core.language();
    let slow = Duration::from_secs(180);

    // Criterion 8: `List` is not imported.
    until("diagnostics", slow, async || {
        language
            .diagnostics(&p.uri)
            .iter()
            .any(|d| d.message.contains("List"))
    })
    .await;

    // Criterion 7: members of System.out.
    let items = language.completion(&p.uri, Position::new(6, 19)).await;
    assert!(
        items.iter().any(|i| i.label.starts_with("println")),
        "{:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>()
    );

    // Criterion 9: greet() is declared below.
    let found = language.definition(&p.uri, Position::new(5, 9)).await;
    assert_eq!(found.first().map(|l| l.range.start.line), Some(9));

    // Criterion 18: the quick fix adds the import.
    let list = Range::new(Position::new(4, 8), Position::new(4, 12));
    let actions = language.code_actions(&p.uri, list).await;
    let import = actions
        .iter()
        .find(|a| a.title.contains("Import") && a.title.contains("java.util"))
        .unwrap_or_else(|| panic!("{:?}", actions.iter().map(|a| &a.title).collect::<Vec<_>>()));
    language.apply_code_action(import).await.unwrap();
    let text = p
        .ide
        .core
        .documents()
        .snapshot(p.doc)
        .unwrap()
        .text
        .to_string();
    assert!(text.contains("import java.util.List;"), "{text}");
    p.ide.core.shutdown().await;
}
