//! E12: the Git plugin (WASM) on a real repository (MVP criterion 13): status through the
//! system `git`, the Commit view, file decorations and the branch item, commit and branch,
//! the interface `kumiko.git.repository` for another plugin, and disabling it leaves the
//! project working.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use kumiko_headless::kumiko_core::plugin_host::PluginStore;
use kumiko_headless::{Headless, eventually};
use kumiko_types::{CommandId, PluginId, Uri};
use serde_json::{Value, json};

use crate::fixtures;

fn component() -> &'static Path {
    static COMPONENT: OnceLock<PathBuf> = OnceLock::new();
    COMPONENT.get_or_init(|| {
        let extra = ["--release", "--target", "wasm32-wasip2"];
        fixtures::build("kumiko-git", &extra, "kumiko_git", ".wasm")
    })
}

fn echo_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| fixtures::build("kumiko-echo", &[], "kumiko-echo", ""))
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

async fn run(ide: &Headless, command: &str, args: Vec<Value>) -> Value {
    let id = CommandId::new(command).unwrap();
    ide.core.commands().execute(&id, args).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_status_commit_branch_and_the_interface() {
    let ide = Headless::start_with(|options| {
        let store = PluginStore::new(&options.dirs);
        let manifest = fixtures::workspace_root().join("plugins/git/plugin.toml");
        let git = fixtures::plugin_source(
            options.dirs.data_dir(),
            "git-src",
            &manifest,
            |m| m,
            &[("plugin.wasm", component())],
        );
        store.install(&git).unwrap();
        let echo_manifest = fixtures::workspace_root().join("sdk/examples/echo/plugin.toml");
        let name = format!("bin/kumiko-echo{}", std::env::consts::EXE_SUFFIX);
        let echo = fixtures::plugin_source(
            options.dirs.data_dir(),
            "echo-src",
            &echo_manifest,
            |m| m,
            &[(name.as_str(), echo_binary())],
        );
        store.install(&echo).unwrap();
    })
    .await
    .unwrap();
    let repo = ide.workspace().to_path_buf();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test"]);
    ide.write("src/App.java", "class App {}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    ide.write("src/App.java", "class App { int x; }\n").unwrap();
    ide.write("notes.txt", "todo\n").unwrap();
    ide.core.open_folder(&repo).await.unwrap();

    // Criterion 13: git status.
    let status = run(&ide, "git.status", vec![]).await;
    assert_eq!(status["branch"], "main");
    let changes: Vec<(String, String)> = status["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().into(),
                f["change"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [
            ("src/App.java".into(), "modified".into()),
            ("notes.txt".into(), "untracked".into())
        ]
    );

    // Declarative UI: the view, the colours, the branch.
    let roots = ide.core.view_tree("git.changes");
    assert_eq!(roots[0].label, "Changes");
    assert_eq!(roots[0].children.as_ref().unwrap()[0].label, "App.java");
    let folder = ide.core.project().folders()[0].uri.clone();
    let app = Uri::parse(format!(
        "{}/src/App.java",
        folder.as_str().trim_end_matches('/')
    ))
    .unwrap();
    let decorations = ide.core.file_decorations();
    assert_eq!(
        decorations[&app][0].color.as_deref(),
        Some("vcs.modified"),
        "{decorations:?}"
    );
    assert!(
        ide.core
            .status_items()
            .iter()
            .any(|i| i.text.contains("main"))
    );

    // Commit (the message as an argument; without one the plugin asks the UI).
    let after = run(&ide, "git.commit", vec![json!("second")]).await;
    let left: Vec<&str> = after["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(left, ["notes.txt"], "the tracked change is committed");
    assert!(
        !ide.core.file_decorations().contains_key(&app),
        "clean again"
    );
    assert_eq!(
        run(&ide, "git.commit", vec![]).await,
        Value::Null,
        "no UI: cancelled"
    );
    run(&ide, "git.createBranch", vec![json!("feature")]).await;

    // Another plugin uses the interface.
    let branch = run(
        &ide,
        "echo.call",
        vec![json!("kumiko.git.repository"), json!("branch"), json!({})],
    )
    .await;
    assert_eq!(branch, json!("feature"));

    // Disabled: its UI goes, the project stays.
    ide.core
        .set_plugin_enabled(&PluginId::new("kumiko.git").unwrap(), false)
        .await
        .unwrap();
    eventually("decorations cleared", || {
        ide.core.file_decorations().is_empty()
    })
    .await
    .unwrap();
    assert!(ide.core.view_tree("git.changes").is_empty());
    eventually("the index still works", || ide.core.file_index().len() >= 2)
        .await
        .unwrap();
}
