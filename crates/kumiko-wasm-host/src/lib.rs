//! WASM runtime for L2w plugins: wasmtime with the Component Model (spec, section 13).
//!
//! - one instance per plugin; calls into it are serialized, re-entrant calls are refused
//!   (components cannot be re-entered);
//! - guests run on the async executor and yield every epoch tick, so a busy plugin never blocks
//!   a thread; a call that runs longer than its budget is interrupted;
//! - linear memory is limited (64 MiB by default, a manifest may ask for up to 256 MiB);
//! - a trap kills the instance only: the host treats it as a crash of the plugin;
//! - compiled components are cached on disk for fast cold starts;
//! - WASI is linked without files, sockets, environment or arguments: plugins reach the
//!   workspace only through the host, which enforces permissions.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Mutex;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{
    Cache, CacheConfig, Config, Engine, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline,
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use kumiko_protocol::{ErrorCode, ResponseError};
use kumiko_service_api::BoxFuture;
use kumiko_service_api::wasm::{
    WasmCallError, WasmHost, WasmInstance, WasmInstanceSpec, WasmRuntime,
};
use kumiko_types::PluginId;

mod bindings {
    wasmtime::component::bindgen!({
        path: "../../sdk/wit/kumiko-plugin",
        world: "kumiko-plugin",
        imports: { default: async | trappable },
        exports: { default: async },
    });
}

use bindings::KumikoPlugin;
use bindings::kumiko::plugin::types::RpcError;

#[derive(Debug, Clone)]
pub struct WasmConfig {
    /// Where compiled components are cached; `None` compiles on every start.
    pub cache_dir: Option<PathBuf>,
    /// Default linear memory limit.
    pub memory_limit: usize,
    /// The most a manifest may ask for.
    pub max_memory_limit: usize,
    /// Running time of one call before it is interrupted (time spent in host calls excluded).
    pub call_budget: Duration,
    /// Epoch tick: how often running guests yield and budgets are checked.
    pub tick: Duration,
}

impl Default for WasmConfig {
    fn default() -> Self {
        Self {
            cache_dir: None,
            memory_limit: 64 << 20,
            max_memory_limit: 256 << 20,
            call_budget: Duration::from_secs(10),
            tick: Duration::from_millis(10),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("wasm engine: {0}")]
pub struct WasmHostError(String);

/// The engine shared by all WASM plugins.
pub struct WasmEngine {
    engine: Engine,
    linker: Arc<Linker<State>>,
    config: WasmConfig,
    stop_ticker: Arc<AtomicBool>,
}

impl Drop for WasmEngine {
    fn drop(&mut self) {
        self.stop_ticker.store(true, Ordering::Relaxed);
    }
}

impl WasmEngine {
    pub fn new(config: WasmConfig) -> Result<Self, WasmHostError> {
        let error = |e: wasmtime::Error| WasmHostError(format!("{e:#}"));
        let mut engine_config = Config::new();
        engine_config.epoch_interruption(true);
        if let Some(dir) = &config.cache_dir {
            let mut cache = CacheConfig::new();
            cache.with_directory(dir);
            engine_config.cache(Some(Cache::new(cache).map_err(error)?));
        }
        let engine = Engine::new(&engine_config).map_err(error)?;

        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(error)?;
        KumikoPlugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state).map_err(error)?;

        // Epoch ticks drive yielding and budgets; the thread ends with the engine.
        let stop = Arc::new(AtomicBool::new(false));
        let (ticking, tick, stopped) = (engine.clone(), config.tick, stop.clone());
        std::thread::Builder::new()
            .name("kumiko-wasm-epoch".into())
            .spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    std::thread::sleep(tick);
                    ticking.increment_epoch();
                }
            })
            .map_err(|e| WasmHostError(e.to_string()))?;

        Ok(Self {
            engine,
            linker: Arc::new(linker),
            config,
            stop_ticker: stop,
        })
    }
}

impl WasmRuntime for WasmEngine {
    fn instantiate(
        &self,
        spec: WasmInstanceSpec,
        host: Arc<dyn WasmHost>,
    ) -> BoxFuture<'static, Result<Arc<dyn WasmInstance>, String>> {
        Box::pin(instantiate(
            self.engine.clone(),
            self.linker.clone(),
            self.config.clone(),
            spec,
            host,
        ))
    }
}

async fn instantiate(
    engine: Engine,
    linker: Arc<Linker<State>>,
    config: WasmConfig,
    spec: WasmInstanceSpec,
    host: Arc<dyn WasmHost>,
) -> Result<Arc<dyn WasmInstance>, String> {
    let path = spec.component.clone();
    let compile_engine = engine.clone();
    // Compilation is CPU-bound and may take seconds without a cache hit.
    let component =
        tokio::task::spawn_blocking(move || Component::from_file(&compile_engine, &path))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("cannot load {}: {e:#}", spec.component.display()))?;

    let memory = spec
        .memory_limit
        .unwrap_or(config.memory_limit)
        .min(config.max_memory_limit);
    let budget = ticks(config.call_budget, config.tick);
    let state = State {
        wasi: WasiCtxBuilder::new().inherit_stderr().build(),
        table: ResourceTable::new(),
        limits: StoreLimitsBuilder::new()
            .memory_size(memory)
            .instances(100)
            .tables(100)
            .memories(10)
            .build(),
        host,
        ticks_left: budget,
    };
    let mut store = Store::new(&engine, state);
    store.limiter(|state| &mut state.limits);
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(|mut ctx| {
        let state = ctx.data_mut();
        if state.ticks_left == 0 {
            return Err(wasmtime::Error::msg("the call ran out of its time budget"));
        }
        state.ticks_left -= 1;
        Ok(UpdateDeadline::Yield(1))
    });
    let bindings = KumikoPlugin::instantiate_async(&mut store, &component, &linker)
        .await
        .map_err(|e| format!("cannot instantiate {}: {e:#}", spec.plugin))?;
    Ok(Arc::new(Handle(Arc::new(Instance {
        plugin: spec.plugin,
        budget,
        live: Mutex::new(Some(Live { store, bindings })),
        dead: AtomicBool::new(false),
    }))))
}

fn ticks(budget: Duration, tick: Duration) -> u64 {
    let tick = tick.as_millis().max(1);
    u64::try_from(budget.as_millis() / tick)
        .unwrap_or(u64::MAX)
        .max(1)
}

/// Store data of one plugin instance.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    host: Arc<dyn WasmHost>,
    /// Epoch ticks the current call may still run.
    ticks_left: u64,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl bindings::kumiko::plugin::types::Host for State {}

impl bindings::kumiko::plugin::host::Host for State {
    async fn call(
        &mut self,
        method: String,
        params: String,
    ) -> wasmtime::Result<Result<String, RpcError>> {
        let params = match serde_json::from_str(&params) {
            Ok(params) => params,
            Err(error) => {
                let error = ResponseError::new(
                    ErrorCode::InvalidParams,
                    format!("params are not JSON: {error}"),
                );
                return Ok(Err(to_rpc_error(error)));
            }
        };
        let result = self.host.call(method, params).await;
        Ok(result.map(|value| value.to_string()).map_err(to_rpc_error))
    }

    async fn notify(&mut self, method: String, params: String) -> wasmtime::Result<()> {
        match serde_json::from_str(&params) {
            Ok(params) => self.host.notify(method, params),
            Err(error) => tracing::warn!(%method, %error, "plugin notification with invalid JSON"),
        }
        Ok(())
    }

    async fn stream_write(
        &mut self,
        _id: u32,
        _data: Vec<u8>,
    ) -> wasmtime::Result<Result<(), RpcError>> {
        Ok(Err(to_rpc_error(ResponseError::new(
            ErrorCode::MethodNotFound,
            "binary streams are not available to WASM plugins yet",
        ))))
    }

    async fn stream_close(&mut self, _id: u32) -> wasmtime::Result<()> {
        Ok(())
    }
}

fn to_rpc_error(error: ResponseError) -> RpcError {
    RpcError {
        code: error.code,
        message: error.message,
        data: error.data.map(|data| data.to_string()),
    }
}

fn from_rpc_error(error: RpcError) -> ResponseError {
    ResponseError {
        code: error.code,
        message: error.message,
        data: error.data.and_then(|data| serde_json::from_str(&data).ok()),
    }
}

struct Live {
    store: Store<State>,
    bindings: KumikoPlugin,
}

struct Instance {
    plugin: PluginId,
    budget: u64,
    /// `None` after a trap.
    live: Mutex<Option<Live>>,
    dead: AtomicBool,
}

tokio::task_local! {
    /// Plugins whose calls are on the current call chain, to refuse re-entrance instead of
    /// deadlocking on the instance lock.
    static ACTIVE: Vec<PluginId>;
}

/// Which export to call.
enum Export {
    Handle,
    Notify,
}

impl Instance {
    /// Calls an export with exclusive access to the live instance; a trap kills it.
    async fn call(
        self: Arc<Self>,
        export: Export,
        method: String,
        params: String,
    ) -> Result<Result<String, RpcError>, WasmCallError> {
        let mut chain = ACTIVE.try_with(Clone::clone).unwrap_or_default();
        if chain.contains(&self.plugin) {
            return Err(WasmCallError::Reentrant(self.plugin.clone()));
        }
        chain.push(self.plugin.clone());
        ACTIVE
            .scope(chain, async move {
                let mut guard = self.live.lock().await;
                let Some(live) = guard.as_mut() else {
                    return Err(WasmCallError::Trap("the instance is dead".into()));
                };
                live.store.data_mut().ticks_left = self.budget;
                let exports = live.bindings.kumiko_plugin_plugin();
                let result = match export {
                    Export::Handle => exports.call_handle(&mut live.store, &method, &params).await,
                    Export::Notify => exports
                        .call_on_notify(&mut live.store, &method, &params)
                        .await
                        .map(|()| Ok(String::new())),
                };
                result.map_err(|error| {
                    *guard = None;
                    self.dead.store(true, Ordering::Relaxed);
                    WasmCallError::Trap(format!("{error:#}"))
                })
            })
            .await
    }
}

/// The instance as the Plugin Host sees it.
struct Handle(Arc<Instance>);

impl WasmInstance for Handle {
    fn handle(
        &self,
        method: String,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, WasmCallError>> {
        let call = self
            .0
            .clone()
            .call(Export::Handle, method, params.to_string());
        Box::pin(async move {
            match call.await? {
                Ok(json) => serde_json::from_str(&json).map_err(|error| {
                    WasmCallError::Rpc(ResponseError::new(
                        ErrorCode::InternalError,
                        format!("the plugin answered with invalid JSON: {error}"),
                    ))
                }),
                Err(error) => Err(WasmCallError::Rpc(from_rpc_error(error))),
            }
        })
    }

    fn notify(
        &self,
        method: String,
        params: Value,
    ) -> BoxFuture<'static, Result<(), WasmCallError>> {
        let call = self
            .0
            .clone()
            .call(Export::Notify, method, params.to_string());
        Box::pin(async move { call.await.map(|_| ()) })
    }

    fn is_dead(&self) -> bool {
        self.0.dead.load(Ordering::Relaxed)
    }
}
