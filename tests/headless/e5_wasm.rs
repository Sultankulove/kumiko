//! E5: the E4 suite for WASM plugins. `kumiko.echo-wasm` is installed at runtime, started on
//! first use, restarted after a trap, interrupted when it loops, limited in memory and refused
//! host methods it has no permission for; the core stays alive throughout. `kumiko.hello-c`, a
//! plugin written in C, covers MVP criterion 16.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Value, json};

use kumiko_headless::kumiko_core::protocol::events::{PluginState, PluginStateChanged};
use kumiko_headless::kumiko_core::service_api::Event;
use kumiko_headless::{Headless, eventually, wait_for};
use kumiko_types::{CommandId, PluginId};

use crate::fixtures;

/// The fixture component, built once per test run.
fn component() -> &'static Path {
    static COMPONENT: OnceLock<PathBuf> = OnceLock::new();
    COMPONENT.get_or_init(|| {
        let extra = ["--release", "--target", "wasm32-wasip2"];
        fixtures::build("kumiko-echo-wasm", &extra, "kumiko_echo_wasm", ".wasm")
    })
}

fn source(dir: &Path, edit: impl FnOnce(String) -> String) -> PathBuf {
    let manifest = fixtures::workspace_root().join("sdk/examples/echo-wasm/plugin.toml");
    let files = [("plugin.wasm", component())];
    fixtures::plugin_source(dir, "echo-wasm-src", &manifest, edit, &files)
}

async fn start(max_crashes: usize) -> Headless {
    Headless::start_with(|options| {
        options.plugins.backoff = vec![Duration::from_millis(50)];
        options.plugins.max_crashes = max_crashes;
        options.wasm.call_budget = Duration::from_millis(300);
    })
    .await
    .unwrap()
}

fn plugin() -> PluginId {
    PluginId::new("kumiko.echo-wasm").unwrap()
}

async fn run(ide: &Headless, command: &str, args: Vec<Value>) -> Result<Value, String> {
    let id = CommandId::new(command).unwrap();
    ide.core
        .commands()
        .execute(&id, args)
        .await
        .map_err(|e| e.to_string())
}

async fn calls(ide: &Headless) -> u64 {
    run(ide, "echo-wasm.count", vec![]).await.unwrap()["calls"]
        .as_u64()
        .unwrap()
}

fn state_is(state: PluginState) -> impl FnMut(&Event) -> bool {
    move |event| {
        event
            .decode::<PluginStateChanged>()
            .and_then(Result::ok)
            .is_some_and(|changed| changed.plugin == plugin() && changed.state == state)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wasm_plugin_runs_crashes_and_recovers() {
    let ide = start(5).await;
    let core = &ide.core;
    let host = core.plugin_host();
    let mut events = ide.subscribe(&["plugin.*"]);

    core.install_plugin(&source(ide.home(), |m| m))
        .await
        .unwrap();
    assert_eq!(host.state(&plugin()), Some(PluginState::Enabled));

    // Started by the first command.
    assert_eq!(
        run(&ide, "echo-wasm.say", vec![json!("hi")]).await.unwrap(),
        json!(["hi"])
    );
    assert_eq!(host.state(&plugin()), Some(PluginState::Running));
    assert_eq!(
        core.commands()
            .context()
            .get("plugin.kumiko.echo-wasm.running"),
        Some(json!(true))
    );
    assert_eq!(calls(&ide).await, 1);
    assert_eq!(calls(&ide).await, 2);

    // A trap is a crash: Crashed, then a fresh instance.
    let trap = run(&ide, "echo-wasm.trap", vec![]).await.unwrap_err();
    assert!(trap.contains("trap"), "{trap}");
    wait_for(&mut events, "Crashed", state_is(PluginState::Crashed))
        .await
        .unwrap();
    wait_for(&mut events, "restart", state_is(PluginState::Running))
        .await
        .unwrap();
    assert_eq!(calls(&ide).await, 1);

    // An endless loop runs out of its budget; meanwhile the core keeps serving.
    let spin = tokio::spawn({
        let core = core.clone();
        async move {
            let id = CommandId::new("echo-wasm.spin").unwrap();
            core.commands().execute(&id, vec![]).await
        }
    });
    assert!(core.config().get("editor.tabSize", None).is_some());
    let error = spin.await.unwrap().unwrap_err().to_string();
    assert!(error.contains("time budget"), "{error}");
    eventually("restart after the loop", || {
        host.state(&plugin()) == Some(PluginState::Running)
    })
    .await
    .unwrap();
    assert_eq!(calls(&ide).await, 1);

    // Graceful stop drops the instance; the next command starts a new one.
    host.stop(&plugin()).await;
    assert_eq!(host.state(&plugin()), Some(PluginState::Enabled));
    assert_eq!(calls(&ide).await, 1);
    ide.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_are_enforced_for_wasm_plugins() {
    let ide = start(3).await;
    let core = &ide.core;
    core.install_plugin(&source(ide.home(), |m| m))
        .await
        .unwrap();

    let host_call = |method: &str| vec![json!(method), json!({})];
    for (method, permission) in [
        ("fs/read", "fs.workspace"),
        ("process/spawn", "process.spawn"),
        ("ui/showMessage", "ui"),
    ] {
        let error = run(&ide, "echo-wasm.host", host_call(method))
            .await
            .unwrap_err();
        assert!(
            error.contains(&format!("has no permission `{permission}`")),
            "{method}: {error}"
        );
    }
    // Methods without a permission pass the check.
    let error = run(&ide, "echo-wasm.host", host_call("command/execute"))
        .await
        .unwrap_err();
    assert!(error.contains("invalid params"), "{error}");

    // With the permission granted, the call reaches the host (not implemented there yet).
    core.uninstall_plugin(&plugin()).await.unwrap();
    let granted = source(ide.home(), |m| {
        m.replace("kind = \"wasm\"", "kind = \"wasm\"\npermissions = [\"ui\"]")
    });
    core.install_plugin(&granted).await.unwrap();
    let error = run(&ide, "echo-wasm.host", host_call("ui/showMessage"))
        .await
        .unwrap_err();
    assert!(error.contains("does not implement"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_limits_and_repeated_traps() {
    let ide = start(3).await;
    let core = &ide.core;
    let limited = source(ide.home(), |m| {
        m.replace("kind = \"wasm\"", "kind = \"wasm\"\nmemory = 16")
    });
    core.install_plugin(&limited).await.unwrap();

    assert_eq!(
        run(&ide, "echo-wasm.grow", vec![json!(4)]).await.unwrap(),
        json!(4)
    );
    let error = run(&ide, "echo-wasm.grow", vec![json!(32)])
        .await
        .unwrap_err();
    assert!(error.contains("trap"), "{error}");

    // Two more crashes make three within the window: Failed.
    for _ in 0..2 {
        let _ = run(&ide, "echo-wasm.trap", vec![]).await.unwrap_err();
    }
    eventually("Failed", || {
        core.plugin_host().state(&plugin()) == Some(PluginState::Failed)
    })
    .await
    .unwrap();
    let error = run(&ide, "echo-wasm.say", vec![]).await.unwrap_err();
    assert!(error.contains("crashed 3 times"), "{error}");
    assert!(core.config().get("editor.tabSize", None).is_some());
}

/// Builds `sdk/examples/hello-c` with wasi-sdk (`WASI_SDK_PATH`, by default ~/.local/wasi-sdk).
fn hello_c(dir: &Path) -> PathBuf {
    let example = fixtures::workspace_root().join("sdk/examples/hello-c");
    let out = dir.join("hello-c-build");
    let status = std::process::Command::new("bash")
        .arg(example.join("build.sh"))
        .arg(&out)
        .status()
        .expect("bash runs");
    assert!(
        status.success(),
        "building the C plugin failed; is wasi-sdk installed?"
    );
    let files = [("plugin.wasm", out.join("plugin.wasm"))];
    let files: Vec<(&str, &Path)> = files.iter().map(|(n, p)| (*n, p.as_path())).collect();
    fixtures::plugin_source(
        dir,
        "hello-c-src",
        &example.join("plugin.toml"),
        |m| m,
        &files,
    )
}

/// MVP criterion 16: install a WASM plugin written in C and call its commands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn criterion_16_plugin_written_in_c() {
    let ide = start(3).await;
    let core = &ide.core;
    let installed = core.install_plugin(&hello_c(ide.home())).await.unwrap();
    assert_eq!(installed.as_str(), "kumiko.hello-c");

    for count in 1..=2 {
        assert_eq!(
            run(&ide, "hello-c.greet", vec![]).await.unwrap(),
            json!({ "greeting": "Hello from C", "count": count })
        );
    }
    // The C plugin calls back into the host.
    assert_eq!(
        run(&ide, "hello-c.remember", vec![]).await.unwrap(),
        json!(true)
    );
    assert_eq!(
        core.commands().context().get("hello-c.greeted"),
        Some(json!(true))
    );
    assert_eq!(
        core.plugin_host().state(&installed),
        Some(PluginState::Running)
    );

    core.uninstall_plugin(&installed).await.unwrap();
    assert!(run(&ide, "hello-c.greet", vec![]).await.is_err());
    assert_eq!(core.commands().context().get("hello-c.greeted"), None);
}
