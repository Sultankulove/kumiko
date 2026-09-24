//! E4: a real process plugin (`kumiko.echo`) is started on first use, survives `kill -9` of its
//! process by being restarted, and the core stays alive (MVP criterion 15 on the fixture).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Value, json};

use kumiko_headless::kumiko_core::plugin_host::PluginStore;
use kumiko_headless::kumiko_core::protocol::events::{PluginState, PluginStateChanged};
use kumiko_headless::kumiko_core::service_api::Event;
use kumiko_headless::{Headless, eventually, wait_for};
use kumiko_types::{CommandId, PluginId};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Builds the fixture once per test run and returns the path of its binary.
fn echo_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let output = Command::new(cargo)
            .args(["build", "-q", "-p", "kumiko-echo", "--message-format=json"])
            .current_dir(workspace_root())
            .output()
            .expect("cargo runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "kumiko-echo")
            .and_then(|m| m["executable"].as_str().map(PathBuf::from))
            .expect("cargo reports the kumiko-echo executable")
    })
}

/// An installable copy of `sdk/examples/echo`: its manifest and the built binary.
fn echo_source(dir: &Path) -> PathBuf {
    let source = dir.join("echo-src");
    std::fs::create_dir_all(source.join("bin")).unwrap();
    std::fs::copy(
        workspace_root().join("sdk/examples/echo/plugin.toml"),
        source.join("plugin.toml"),
    )
    .unwrap();
    std::fs::copy(echo_binary(), source.join("bin/kumiko-echo")).unwrap();
    source
}

async fn start(max_crashes: usize) -> Headless {
    Headless::start_with(|options| {
        let source = echo_source(options.dirs.data_dir());
        PluginStore::new(&options.dirs).install(&source).unwrap();
        options.plugins.backoff = vec![Duration::from_millis(50)];
        options.plugins.max_crashes = max_crashes;
    })
    .await
    .unwrap()
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

async fn pid(ide: &Headless) -> u64 {
    run(ide, "echo.whoami", vec![]).await.unwrap()["pid"]
        .as_u64()
        .unwrap()
}

fn state_is(state: PluginState) -> impl FnMut(&Event) -> bool {
    move |event| {
        event
            .decode::<PluginStateChanged>()
            .and_then(Result::ok)
            .is_some_and(|changed| changed.plugin == echo() && changed.state == state)
    }
}

fn alive(pid: u64) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_9_restarts_the_plugin_and_the_core_survives() {
    let ide = start(3).await;
    let host = ide.core.plugin_host();
    let mut events = ide.subscribe(&["plugin.*"]);

    // Installed and enabled, but no process until the first command.
    assert_eq!(host.state(&echo()), Some(PluginState::Enabled));
    assert_eq!(host.pid(&echo()), None);

    assert_eq!(
        run(&ide, "echo.say", vec![json!("hi"), json!(2)])
            .await
            .unwrap(),
        json!(["hi", 2])
    );
    assert_eq!(host.state(&echo()), Some(PluginState::Running));
    assert_eq!(
        ide.core
            .commands()
            .context()
            .get("plugin.kumiko.echo.running"),
        Some(json!(true))
    );
    let first = pid(&ide).await;
    assert_eq!(host.pid(&echo()).map(u64::from), Some(first));

    // kill -9: Crashed, then restarted with a new process.
    let status = Command::new("kill")
        .args(["-9", &first.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    wait_for(&mut events, "Crashed", state_is(PluginState::Crashed))
        .await
        .unwrap();
    wait_for(&mut events, "restart", state_is(PluginState::Running))
        .await
        .unwrap();
    let second = pid(&ide).await;
    assert_ne!(first, second);
    assert!(!alive(first));

    // The core is alive and serves other requests.
    assert!(ide.core.config().get("editor.tabSize", None).is_some());
    assert_eq!(
        run(&ide, "echo.say", vec![json!("again")]).await.unwrap(),
        json!(["again"])
    );

    // Graceful stop: shutdown → exit, the process is gone and the plugin can start again.
    host.stop(&echo()).await;
    assert_eq!(host.state(&echo()), Some(PluginState::Enabled));
    eventually("the process exits", || !alive(second))
        .await
        .unwrap();
    assert_eq!(
        ide.core
            .commands()
            .context()
            .get("plugin.kumiko.echo.running"),
        Some(json!(false))
    );
    assert_ne!(pid(&ide).await, second);
    ide.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_crashes_make_the_plugin_failed() {
    let ide = start(3).await;
    let host = ide.core.plugin_host();

    for crash in 1..=3 {
        let error = run(&ide, "echo.exit", vec![json!(7)]).await.unwrap_err();
        assert!(
            error.contains("exited during the call"),
            "crash {crash}: {error}"
        );
    }
    eventually("Failed", || {
        host.state(&echo()) == Some(PluginState::Failed)
    })
    .await
    .unwrap();
    let error = run(&ide, "echo.say", vec![]).await.unwrap_err();
    assert!(error.contains("crashed 3 times"), "{error}");
    // The rest of the core keeps working.
    assert!(ide.core.config().get("editor.tabSize", None).is_some());
}
