//! Running WASM plugin components (spec, section 13; ADR-019). The Plugin Host starts plugins
//! through [`WasmRuntime`] without knowing the runtime; `kumiko-wasm-host` implements it.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use kumiko_protocol::ResponseError;
use kumiko_types::PluginId;

use crate::BoxFuture;

/// Compiles and instantiates plugin components.
pub trait WasmRuntime: Send + Sync {
    /// Compiles the component (or takes it from the cache) and instantiates it. Nothing of
    /// the plugin runs yet: the host sends `initialize` through [`WasmInstance::handle`].
    fn instantiate(
        &self,
        spec: WasmInstanceSpec,
        host: Arc<dyn WasmHost>,
    ) -> BoxFuture<'static, Result<Arc<dyn WasmInstance>, String>>;
}

/// What to instantiate.
#[derive(Debug, Clone)]
pub struct WasmInstanceSpec {
    pub plugin: PluginId,
    /// The `.wasm` component.
    pub component: PathBuf,
    /// Linear memory limit in bytes; `None` takes the runtime default.
    pub memory_limit: Option<usize>,
}

/// Calls of the component into the host: the imports of the WIT world.
pub trait WasmHost: Send + Sync {
    /// `host.call`: a plugin-to-host KPP request.
    fn call(
        &self,
        method: String,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, ResponseError>>;
    /// `host.notify`: a plugin-to-host KPP notification.
    fn notify(&self, method: String, params: Value);
}

/// A running component: the exports of the WIT world.
pub trait WasmInstance: Send + Sync {
    /// `plugin.handle`: a host-to-plugin KPP request.
    fn handle(
        &self,
        method: String,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, WasmCallError>>;
    /// `plugin.on-notify`: a host-to-plugin KPP notification.
    fn notify(
        &self,
        method: String,
        params: Value,
    ) -> BoxFuture<'static, Result<(), WasmCallError>>;
    /// Whether the instance trapped and can no longer be used.
    fn is_dead(&self) -> bool;
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WasmCallError {
    /// The plugin answered with an error.
    #[error("{0}")]
    Rpc(ResponseError),
    /// The plugin trapped (a crash, a timeout, a memory limit); the instance is dead.
    #[error("trap: {0}")]
    Trap(String),
    /// A call into a plugin that is itself waiting for this call chain; components cannot be
    /// re-entered.
    #[error("re-entrant call into plugin {0}")]
    Reentrant(PluginId),
}
