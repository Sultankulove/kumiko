//! Modularity tests (spec, section 29): the IDE keeps working when parts are removed, killed
//! or replaced. Tests 7, 8, 10 and 11 of the list are covered by suites of earlier stages;
//! `docs/acceptance/e14-mvp.md` says which.
//!
//! 1. no Java plugin: `.java` opens as text            -> `without_the_java_plugin`
//! 2. no Git plugin: tree and terminal work            -> `without_the_git_plugin`
//! 3. `kill -9` of a plugin during a request           -> `killing_a_plugin_during_a_request`
//! 4. `kill -9` of the language server                 -> `killing_the_language_server`
//! 5. the `alt-editor` profile builds                  -> `the_alt_editor_profile_builds`
//! 6. no terminal plugin: tasks still run              -> `tasks_run_without_the_terminal_plugin`
//! 7. WASM trap, 8. WASM endless loop                  -> headless `e5_wasm`
//! 9. kits `minimal`, `java`, `minimal`                -> `kits_leave_user_settings_alone`
//! 10. reference plugin in C                           -> headless `e5_wasm::criterion_16...`
//! 11. subscriber queue overflow, resync               -> `kumiko-events` tests

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../headless/fixtures.rs"]
mod fixtures;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use kumiko_headless::kumiko_core::language::ServerStatus;
use kumiko_headless::kumiko_core::plugin_host::PluginStore;
use kumiko_headless::kumiko_core::protocol::events::{PluginState, PluginStateChanged};
use kumiko_headless::kumiko_core::service_api::{Event, ProcessSpec, PtySize};
use kumiko_headless::{Headless, eventually, wait_for};
use kumiko_types::{CommandId, PluginId, Position, Range, TextEdit, Uri};
use serde_json::{Value, json};

fn binary(package: &'static str) -> PathBuf {
    fixtures::build(package, &[], package, "")
}

/// `kumiko.echo` with the fake language server next to it (as in the Language service tests).
fn echo_source(dir: &Path) -> PathBuf {
    static BINARIES: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    let (echo, server) =
        BINARIES.get_or_init(|| (binary("kumiko-echo"), binary("kumiko-fake-lsp")));
    let manifest = fixtures::workspace_root().join("sdk/examples/echo/plugin.toml");
    let suffix = std::env::consts::EXE_SUFFIX;
    let echo_name = format!("bin/kumiko-echo{suffix}");
    let server_name = format!("bin/kumiko-fake-lsp{suffix}");
    let files = [
        (echo_name.as_str(), echo.as_path()),
        (server_name.as_str(), server.as_path()),
    ];
    fixtures::plugin_source(dir, "echo-src", &manifest, |m| m, &files)
}

fn echo() -> PluginId {
    PluginId::new("kumiko.echo").unwrap()
}

async fn run(ide: &Headless, command: &str, args: Vec<Value>) -> Result<Value, String> {
    let id = CommandId::new(command).unwrap();
    ide.core
        .commands()
        .execute(&id, args)
        .await
        .map_err(|e| e.to_string())
}

fn kill_9(pid: u64) {
    let pid = pid.to_string();
    let status = if cfg!(windows) {
        Command::new("taskkill").args(["/F", "/PID", &pid]).status()
    } else {
        Command::new("kill").args(["-9", &pid]).status()
    };
    assert!(status.unwrap().success());
}

fn state_is(state: PluginState) -> impl FnMut(&Event) -> bool {
    move |event| {
        event
            .decode::<PluginStateChanged>()
            .and_then(Result::ok)
            .is_some_and(|changed| changed.plugin == echo() && changed.state == state)
    }
}

/// 1. Without the Java plugin a `.java` file is text with highlighting from the grammar
/// package; nothing fails, nothing starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_java_plugin() {
    let ide = Headless::start().await.unwrap();
    let path = ide.write("src/App.java", "class App { int x; }\n").unwrap();
    ide.core.open_folder(ide.workspace()).await.unwrap();
    let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
    let doc = ide.core.documents().open(&uri, None).await.unwrap();
    assert_eq!(ide.core.language_of(doc.id).as_deref(), Some("java"));
    assert!(ide.core.plugins().entries().next().is_none());
    assert!(ide.core.language().servers().is_empty());
    assert!(ide.core.language().diagnostics(&uri).is_empty());
    assert!(
        ide.core
            .highlights(doc.id, 0..doc.text.len_bytes())
            .iter()
            .any(|h| h.capture.starts_with("keyword")),
        "highlighting comes from the grammar, not the plugin"
    );
    // Editing and saving work.
    let version = ide.core.documents().snapshot(doc.id).unwrap().version;
    ide.core
        .documents()
        .apply(
            doc.id,
            version,
            vec![TextEdit {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                new_text: "// hi\n".into(),
            }],
        )
        .unwrap();
    ide.core.documents().save(doc.id).await.unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().starts_with("// hi"));
}

/// 2. Without the Git plugin the project tree's index and terminals work, no decorations
/// exist, and a plugin that consumes `kumiko.git.repository` optionally keeps working.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_git_plugin() {
    let ide = Headless::start_with(|options| {
        let source = echo_source(options.dirs.data_dir());
        PluginStore::new(&options.dirs).install(&source).unwrap();
    })
    .await
    .unwrap();
    ide.write("src/a.txt", "a").unwrap();
    ide.core.open_folder(ide.workspace()).await.unwrap();
    eventually("indexed", || !ide.core.file_index().is_empty())
        .await
        .unwrap();
    assert!(ide.core.file_decorations().is_empty());
    assert!(ide.core.view_tree("git.changes").is_empty());

    // The consumer works; asking for the missing interface is an error, not a crash.
    assert_eq!(
        run(&ide, "echo.say", vec![json!("ok")]).await.unwrap(),
        json!(["ok"])
    );
    let error = run(
        &ide,
        "echo.call",
        vec![json!("kumiko.git.repository"), json!("branch"), json!({})],
    )
    .await
    .unwrap_err();
    assert!(error.contains("no provider"), "{error}");
    assert_eq!(
        run(&ide, "echo.say", vec![json!("still")]).await.unwrap(),
        json!(["still"])
    );

    // Terminals do not depend on it.
    let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
    let id = ide
        .core
        .open_terminal(
            "Local",
            ProcessSpec {
                command: shell.into(),
                ..ProcessSpec::default()
            },
            PtySize { cols: 80, rows: 24 },
        )
        .unwrap();
    assert_eq!(ide.core.terminals().len(), 1);
    ide.core.close_terminal(id);
}

/// 3. `kill -9` of a plugin while it handles a request: the request fails, the document keeps
/// its edits, the plugin comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killing_a_plugin_during_a_request() {
    let ide = Headless::start_with(|options| {
        let source = echo_source(options.dirs.data_dir());
        PluginStore::new(&options.dirs).install(&source).unwrap();
        options.plugins.backoff = vec![Duration::from_millis(50)];
    })
    .await
    .unwrap();
    let path = ide.write("notes.txt", "text\n").unwrap();
    let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
    let doc = ide.core.documents().open(&uri, None).await.unwrap();
    ide.core
        .documents()
        .apply(
            doc.id,
            doc.version,
            vec![TextEdit {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                new_text: "unsaved ".into(),
            }],
        )
        .unwrap();

    let mut events = ide.subscribe(&["plugin.*"]);
    let pid = run(&ide, "echo.whoami", vec![]).await.unwrap()["pid"]
        .as_u64()
        .unwrap();
    let hanging = {
        let core = ide.core.clone();
        tokio::spawn(async move {
            let id = CommandId::new("echo.hang").unwrap();
            core.commands().execute(&id, vec![]).await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    kill_9(pid);
    assert!(
        hanging.await.unwrap().is_err(),
        "the request ends with an error"
    );
    wait_for(&mut events, "Crashed", state_is(PluginState::Crashed))
        .await
        .unwrap();
    wait_for(&mut events, "restarted", state_is(PluginState::Running))
        .await
        .unwrap();

    let text = ide
        .core
        .documents()
        .snapshot(doc.id)
        .unwrap()
        .text
        .to_string();
    assert_eq!(text, "unsaved text\n", "no edit is lost");
    assert!(run(&ide, "echo.say", vec![json!(1)]).await.is_ok());
}

/// 4. `kill -9` of the language server: it comes back and diagnostics are restored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killing_the_language_server() {
    let ide = Headless::start_with(|options| {
        let source = echo_source(options.dirs.data_dir());
        PluginStore::new(&options.dirs).install(&source).unwrap();
    })
    .await
    .unwrap();
    ide.core.open_folder(ide.workspace()).await.unwrap();
    let path = ide.write("src/a.fake", "hello TODO\n").unwrap();
    let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
    ide.core.documents().open(&uri, None).await.unwrap();
    let language = ide.core.language();
    eventually("diagnostics", || language.diagnostics(&uri).len() == 1)
        .await
        .unwrap();
    let key = language.servers()[0].0.clone();
    let first = language.server_pid(&key).unwrap();

    kill_9(u64::from(first));
    eventually("the server restarted", || {
        language.server_pid(&key).is_some_and(|pid| pid != first)
            && language.servers()[0].1 == ServerStatus::Running
            && language.diagnostics(&uri).len() == 1
    })
    .await
    .unwrap();
}

/// 5. The `alt-editor` profile builds with a stub editor and no change to other crates.
#[test]
fn the_alt_editor_profile_builds() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "check",
            "-q",
            "-p",
            "kumiko",
            "--no-default-features",
            "--features",
            "alt-editor",
        ])
        .current_dir(fixtures::workspace_root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 6. Tasks run and print into Output without the terminal plugin (it is not even installed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasks_run_without_the_terminal_plugin() {
    let ide = Headless::start_with(|options| {
        let source = echo_source(options.dirs.data_dir());
        PluginStore::new(&options.dirs).install(&source).unwrap();
    })
    .await
    .unwrap();
    assert!(
        ide.core
            .plugins()
            .get(&PluginId::new("kumiko.terminal").unwrap())
            .is_none()
    );
    let task = ide
        .core
        .tasks()
        .await
        .into_iter()
        .find(|t| t.definition.label == "echo: hello")
        .unwrap();
    let channel = task.channel();
    ide.core.run_task(task).await.unwrap();
    eventually("the task output", || {
        ide.core
            .output()
            .lines(&channel)
            .iter()
            .any(|l| l == "hello from a task")
    })
    .await
    .unwrap();
}

/// 9. `minimal`, then `java`, then `minimal`: the plugins follow the kit, the user's settings
/// file is not touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kits_leave_user_settings_alone() {
    let ide = Headless::start_with(|options| {
        let source = echo_source(options.dirs.data_dir());
        PluginStore::new(&options.dirs).install(&source).unwrap();
        let file = options.dirs.user_settings_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "\"editor.tabSize\" = 3\n").unwrap();
    })
    .await
    .unwrap();
    let core = &ide.core;
    let file = core.dirs().user_settings_file();
    let before = std::fs::read_to_string(&file).unwrap();

    let ready = || core.plugins().get(&echo()).is_some_and(|e| e.is_ready());
    assert!(ready());
    core.apply_kit(&core.load_kit("minimal").unwrap())
        .await
        .unwrap();
    assert!(!ready(), "minimal disables every plugin");
    // `java` wants plugins that are not here: reported, applied as far as possible.
    let plan = core
        .apply_kit(&core.load_kit("java").unwrap())
        .await
        .unwrap();
    assert_eq!(plan.missing.len(), 3);
    assert!(
        !ready(),
        "java is not exclusive: the echo plugin stays as it was"
    );
    core.apply_kit(&core.load_kit("minimal").unwrap())
        .await
        .unwrap();
    assert!(!ready());
    assert_eq!(core.active_kit().as_deref(), Some("kumiko.kit.minimal"));

    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    assert_eq!(core.config().get("editor.tabSize", None), Some(json!(3)));
}
