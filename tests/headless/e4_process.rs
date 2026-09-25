//! E4: a real process plugin (`kumiko.echo`) is started on first use, survives `kill -9` of its
//! process by being restarted, and the core stays alive (MVP criterion 15 on the fixture).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Value, json};

use kumiko_headless::kumiko_core::plugin_host::{PluginStatus, PluginStore};
use kumiko_headless::kumiko_core::protocol::events::{PluginState, PluginStateChanged};
use kumiko_headless::kumiko_core::service_api::Event;
use kumiko_headless::{Headless, eventually, wait_for};
use kumiko_types::{CommandId, PluginId};

use crate::fixtures;

/// Builds the fixture once per test run and returns the path of its binary.
fn echo_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| fixtures::build("kumiko-echo", &[], "kumiko-echo", ""))
}

/// An installable copy of `sdk/examples/echo`: its manifest and the built binary.
fn echo_source(dir: &Path) -> PathBuf {
    echo_source_with(dir, |manifest| manifest)
}

/// Like [`echo_source`], with an edited manifest.
fn echo_source_with(dir: &Path, edit: impl FnOnce(String) -> String) -> PathBuf {
    let manifest = fixtures::workspace_root().join("sdk/examples/echo/plugin.toml");
    let files = [("bin/kumiko-echo", echo_binary())];
    fixtures::plugin_source(dir, "echo-src", &manifest, edit, &files)
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

fn command(id: &str) -> CommandId {
    CommandId::new(id).unwrap()
}

/// MVP criterion 14 on the fixture: install, disable, enable and uninstall while the IDE runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_disable_enable_uninstall_at_runtime() {
    let ide = Headless::start().await.unwrap();
    let core = &ide.core;
    let context = || core.commands().context();
    let available = "interface.kumiko.echo.api.available";
    let mut events = ide.subscribe(&["plugin.*"]);
    assert!(!core.commands().contains(&command("echo.say")));

    let installed = core.install_plugin(&echo_source(ide.home())).await.unwrap();
    assert_eq!(installed, echo());
    wait_for(&mut events, "Enabled", state_is(PluginState::Enabled))
        .await
        .unwrap();
    assert_eq!(
        run(&ide, "echo.say", vec![json!(1)]).await.unwrap(),
        json!([1])
    );
    assert_eq!(context().get(available), Some(json!(true)));
    let first = pid(&ide).await;

    // Interfaces are routed through the core: here the plugin calls the interface it provides.
    let call =
        |interface: &str, method: &str| vec![json!(interface), json!(method), json!({ "n": 1 })];
    assert_eq!(
        run(&ide, "echo.call", call("kumiko.echo.api", "ping"))
            .await
            .unwrap(),
        json!({ "pong": { "n": 1 }, "caller": "kumiko.echo" })
    );
    let undeclared = run(&ide, "echo.call", call("kumiko.other.api", "ping"))
        .await
        .unwrap_err();
    assert!(undeclared.contains("does not declare"), "{undeclared}");
    let unknown = run(&ide, "echo.call", call("kumiko.echo.api", "nope"))
        .await
        .unwrap_err();
    assert!(unknown.contains("is not provided"), "{unknown}");

    // Disable: the process stops, contributions and the interface disappear.
    core.set_plugin_enabled(&echo(), false).await.unwrap();
    wait_for(&mut events, "Disabled", state_is(PluginState::Disabled))
        .await
        .unwrap();
    assert!(!core.commands().contains(&command("echo.say")));
    assert_eq!(
        core.plugins().get(&echo()).unwrap().status,
        PluginStatus::Disabled
    );
    assert_eq!(context().get(available), Some(json!(false)));
    eventually("the disabled plugin exits", || !alive(first))
        .await
        .unwrap();

    // Enable again: available without restarting the IDE.
    core.set_plugin_enabled(&echo(), true).await.unwrap();
    assert_eq!(
        run(&ide, "echo.say", vec![json!(2)]).await.unwrap(),
        json!([2])
    );
    let second = pid(&ide).await;

    // Uninstall: stopped, unregistered, files removed; the core keeps working.
    core.uninstall_plugin(&echo()).await.unwrap();
    assert!(!core.commands().contains(&command("echo.say")));
    assert!(core.plugins().get(&echo()).is_none());
    assert!(!core.dirs().plugins_dir().join("kumiko.echo").exists());
    eventually("the uninstalled plugin exits", || !alive(second))
        .await
        .unwrap();
    assert_eq!(context().get("plugin.kumiko.echo.running"), None);
    assert!(core.config().get("editor.tabSize", None).is_some());
    assert!(core.uninstall_plugin(&echo()).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interface_version_ranges_are_enforced() {
    let ide = Headless::start().await.unwrap();
    let source = echo_source_with(ide.home(), |manifest| {
        manifest.replace("version = \"^1\"", "version = \"^2\"")
    });
    ide.core.install_plugin(&source).await.unwrap();
    let args = vec![json!("kumiko.echo.api"), json!("ping"), json!(null)];
    let error = run(&ide, "echo.call", args).await.unwrap_err();
    assert!(
        error.contains("no provider of kumiko.echo.api matches ^2"),
        "{error}"
    );
    ide.core.shutdown().await;
}
