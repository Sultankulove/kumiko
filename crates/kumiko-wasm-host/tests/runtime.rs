//! The runtime against a real component: `sdk/examples/echo-wasm`, built for wasm32-wasip2.

// Test harness: helpers outside `#[test]` functions may panic too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use kumiko_protocol::{ErrorCode, PROTOCOL_VERSION, ResponseError};
use kumiko_service_api::BoxFuture;
use kumiko_service_api::wasm::{
    WasmCallError, WasmHost, WasmInstance, WasmInstanceSpec, WasmRuntime,
};
use kumiko_types::PluginId;
use kumiko_wasm_host::{WasmConfig, WasmEngine};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the fixture component once per test run.
fn component() -> &'static Path {
    static COMPONENT: OnceLock<PathBuf> = OnceLock::new();
    COMPONENT.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let output = Command::new(cargo)
            .args(["build", "-q", "--release", "-p", "kumiko-echo-wasm"])
            .args(["--target", "wasm32-wasip2", "--message-format=json"])
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
            .filter(|m| m["reason"] == "compiler-artifact")
            .filter(|m| m["target"]["name"] == "kumiko_echo_wasm")
            .flat_map(|m| m["filenames"].as_array().cloned().unwrap_or_default())
            .filter_map(|f| f.as_str().map(PathBuf::from))
            .find(|f| f.extension().is_some_and(|e| e == "wasm"))
            .expect("cargo reports the component")
    })
}

/// Answers `test/echo`, calls back into the plugin on `test/reenter`, refuses the rest.
#[derive(Default)]
struct FakeHost {
    instance: OnceLock<Arc<dyn WasmInstance>>,
}

impl WasmHost for FakeHost {
    fn call(
        &self,
        method: String,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, ResponseError>> {
        let instance = self.instance.get().cloned();
        Box::pin(async move {
            match method.as_str() {
                "test/echo" => Ok(json!({ "method": method, "params": params })),
                "test/reenter" => {
                    let instance = instance.expect("instance is set");
                    instance
                        .handle(
                            "command/invoke".into(),
                            json!({ "command": "echo-wasm.say" }),
                        )
                        .await
                        .map_err(|e| ResponseError::new(ErrorCode::InternalError, e.to_string()))
                }
                _ => Err(ResponseError::new(ErrorCode::PermissionDenied, method)),
            }
        })
    }

    fn notify(&self, _method: String, _params: Value) {}
}

fn config() -> WasmConfig {
    WasmConfig {
        memory_limit: 16 << 20,
        call_budget: Duration::from_millis(500),
        ..WasmConfig::default()
    }
}

async fn start(engine: &WasmEngine) -> (Arc<dyn WasmInstance>, Arc<FakeHost>) {
    let host = Arc::new(FakeHost::default());
    let spec = WasmInstanceSpec {
        plugin: PluginId::new("kumiko.echo-wasm").unwrap(),
        component: component().to_owned(),
        memory_limit: None,
    };
    let instance = engine.instantiate(spec, host.clone()).await.unwrap();
    let _ = host.instance.set(instance.clone());
    let init = json!({
        "apiVersion": PROTOCOL_VERSION,
        "pluginId": "kumiko.echo-wasm",
        "hostCapabilities": { "methods": [] },
        "workspace": { "folders": [] },
        "config": {},
        "locale": "en",
    });
    let result = instance.handle("initialize".into(), init).await.unwrap();
    assert_eq!(result["apiVersion"], PROTOCOL_VERSION);
    (instance, host)
}

async fn invoke(
    instance: &Arc<dyn WasmInstance>,
    command: &str,
    args: Value,
) -> Result<Value, WasmCallError> {
    instance
        .handle(
            "command/invoke".into(),
            json!({ "command": command, "args": args }),
        )
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_host_calls_and_reentrance() {
    let engine = WasmEngine::new(config()).unwrap();
    let (instance, _host) = start(&engine).await;

    assert_eq!(
        invoke(&instance, "echo-wasm.say", json!([1, "a"]))
            .await
            .unwrap(),
        json!([1, "a"])
    );
    for expected in 1..=3 {
        let count = invoke(&instance, "echo-wasm.count", json!([]))
            .await
            .unwrap();
        assert_eq!(
            count,
            json!({ "id": "kumiko.echo-wasm", "calls": expected })
        );
    }
    let unknown = invoke(&instance, "echo-wasm.nope", json!([]))
        .await
        .unwrap_err();
    assert!(
        matches!(unknown, WasmCallError::Rpc(ref e) if e.error_code() == Some(ErrorCode::NotFound))
    );

    // Calls into the host, with its errors passed through.
    let echoed = invoke(
        &instance,
        "echo-wasm.host",
        json!(["test/echo", { "a": 1 }]),
    )
    .await
    .unwrap();
    assert_eq!(
        echoed,
        json!({ "method": "test/echo", "params": { "a": 1 } })
    );
    let denied = invoke(&instance, "echo-wasm.host", json!(["fs/read", {}]))
        .await
        .unwrap_err();
    assert!(
        matches!(denied, WasmCallError::Rpc(ref e) if e.error_code() == Some(ErrorCode::PermissionDenied))
    );

    // A host call that re-enters the plugin is refused instead of deadlocking.
    let reentered = invoke(&instance, "echo-wasm.host", json!(["test/reenter", null]))
        .await
        .unwrap_err();
    assert!(reentered.to_string().contains("re-entrant"), "{reentered}");
    assert!(!instance.is_dead());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn traps_kill_only_their_instance() {
    let engine = WasmEngine::new(config()).unwrap();
    let (crashing, _) = start(&engine).await;
    let (healthy, _) = start(&engine).await;

    let trap = invoke(&crashing, "echo-wasm.trap", json!([]))
        .await
        .unwrap_err();
    assert!(matches!(trap, WasmCallError::Trap(_)), "{trap:?}");
    assert!(crashing.is_dead());
    assert!(matches!(
        invoke(&crashing, "echo-wasm.say", json!([])).await,
        Err(WasmCallError::Trap(_))
    ));

    assert_eq!(
        invoke(&healthy, "echo-wasm.say", json!([2])).await.unwrap(),
        json!([2])
    );
    assert!(!healthy.is_dead());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn endless_loops_run_out_of_budget_without_blocking_others() {
    let engine = WasmEngine::new(config()).unwrap();
    let (spinning, _) = start(&engine).await;
    let (healthy, _) = start(&engine).await;

    let started = Instant::now();
    let spin = tokio::spawn({
        let spinning = spinning.clone();
        async move { invoke(&spinning, "echo-wasm.spin", json!([])).await }
    });
    // The loop yields every tick, so another plugin is served meanwhile.
    assert_eq!(
        invoke(&healthy, "echo-wasm.say", json!([3])).await.unwrap(),
        json!([3])
    );
    let error = spin.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("time budget"), "{error}");
    assert!(started.elapsed() >= Duration::from_millis(400));
    assert!(spinning.is_dead());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_is_limited() {
    let engine = WasmEngine::new(config()).unwrap();
    let (instance, _) = start(&engine).await;
    assert_eq!(
        invoke(&instance, "echo-wasm.grow", json!([4]))
            .await
            .unwrap(),
        json!(4)
    );
    let error = invoke(&instance, "echo-wasm.grow", json!([32]))
        .await
        .unwrap_err();
    assert!(matches!(error, WasmCallError::Trap(_)), "{error:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compiled_components_are_cached() {
    let cache = tempfile::tempdir().unwrap();
    let engine = WasmEngine::new(WasmConfig {
        cache_dir: Some(cache.path().to_owned()),
        ..config()
    })
    .unwrap();
    start(&engine).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    let files = || walk(cache.path());
    while files() == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(files() > 0, "nothing was cached");
    // A second engine starts from the cache.
    let again = WasmEngine::new(WasmConfig {
        cache_dir: Some(cache.path().to_owned()),
        ..config()
    })
    .unwrap();
    start(&again).await;
}

fn walk(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| {
                    if e.path().is_dir() {
                        walk(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}
