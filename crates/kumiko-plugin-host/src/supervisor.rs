//! Running plugins: activation, the KPP bridge and supervision (spec, section 13).
//!
//! A plugin with code (a process or a WASM component) starts on first use and is initialized
//! with a timeout. A process exit or a WASM trap without a stop request is a crash: the host
//! releases what the plugin registered at runtime and restarts it with backoff; after too many
//! crashes in a window the plugin is Failed until the IDE restarts. Stopping goes `shutdown` →
//! `exit`, then for processes SIGTERM → SIGKILL; each process plugin runs in its own process
//! group so its children go down with it.
//!
//! Plugin-to-host requests of both kinds go through one bridge. For WASM plugins the
//! permissions of the manifest are enforced there; for process plugins they are declarative
//! (spec, section 13, permissions table).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};

use kumiko_ipc::{Handler, HandlerFuture, Peer, PeerConfig, RpcError};
use kumiko_protocol::common::CommandCall;
use kumiko_protocol::events::{PluginState, PluginStateChanged};
use kumiko_protocol::methods::command::{ExecuteCommand, InvokeCommand, SetContext};
use kumiko_protocol::methods::lifecycle::{
    Exit, HostCapabilities, Initialize, InitializeParams, Initialized, Shutdown, WorkspaceInfo,
};
use kumiko_protocol::methods::lsp::{
    LaunchSpec, LspApplyEdit, LspApplyEditParams, LspMessageParams, LspMiddleware, LspSendRequest,
    MiddlewareResult, ResolveLaunch, ResolveLaunchParams,
};
use kumiko_protocol::methods::service::{
    ServiceCall, ServiceCallParams, ServiceInvoke, ServiceInvokeParams,
};
use kumiko_protocol::methods::task::{
    TaskDefinition, TaskList, TaskParams, TaskProvide, TaskProvideParams, TaskResolve,
};
use kumiko_protocol::{ErrorCode, Notification, Request, ResponseError};
use kumiko_service_api::wasm::{
    WasmCallError, WasmHost, WasmInstance, WasmInstanceSpec, WasmRuntime,
};
use kumiko_service_api::{BoxFuture, Event, EventSink};
use kumiko_types::{InterfaceId, PluginId};

use crate::manifest::{Manifest, Permission, PluginKind};
use crate::registry::{PluginEntry, PluginRegistry, RegistryDiff, protocol_version};

/// What the host needs from the rest of the core; implemented by `kumiko-core`.
pub trait HostApi: Send + Sync + 'static {
    /// `command/execute` from a plugin.
    fn execute_command(
        &self,
        caller: &PluginId,
        call: CommandCall,
    ) -> BoxFuture<'static, Result<Value, ResponseError>>;

    /// `command/setContext`; the key is already checked against the plugin's namespace.
    fn set_context(&self, key: String, value: Value);

    /// Data for `initialize`.
    fn session(&self, plugin: &Manifest) -> Session;

    /// The plugin changed state. On anything but Running its runtime registrations are gone.
    fn state_changed(&self, plugin: &Manifest, state: PluginState);

    /// The user's choice among several providers (`interfaces.<id>.provider`).
    fn preferred_provider(&self, interface: &InterfaceId) -> Option<PluginId>;

    /// `lsp/applyEdit`: a workspace edit of a server the plugin contributes.
    fn lsp_apply_edit(
        &self,
        caller: &PluginId,
        params: LspApplyEditParams,
    ) -> BoxFuture<'static, Result<bool, ResponseError>>;

    /// `lsp/sendRequest`: a request to a language server the plugin contributes.
    fn lsp_send_request(
        &self,
        caller: &PluginId,
        params: LspMessageParams,
    ) -> BoxFuture<'static, Result<Value, ResponseError>>;
}

/// Per-plugin part of `initialize`.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub workspace: WorkspaceInfo,
    /// Current values of the plugin's own settings.
    pub config: Value,
    pub locale: String,
    pub storage: Option<kumiko_protocol::methods::lifecycle::PluginStorage>,
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// From spawn to the `initialize` result of a process plugin.
    pub start_timeout: Duration,
    /// Answer to `initialize` of an instantiated WASM plugin.
    pub wasm_start_timeout: Duration,
    /// Answer to `shutdown`.
    pub shutdown_timeout: Duration,
    /// Grace after `exit` and after SIGTERM before the next step.
    pub terminate_timeout: Duration,
    /// Delay before the n-th restart; the last value repeats.
    pub backoff: Vec<Duration>,
    /// Crashes within `crash_window` that make the plugin Failed.
    pub max_crashes: usize,
    pub crash_window: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            start_timeout: Duration::from_secs(10),
            wasm_start_timeout: Duration::from_secs(2),
            shutdown_timeout: Duration::from_secs(3),
            terminate_timeout: Duration::from_secs(2),
            backoff: [1, 4, 16].map(Duration::from_secs).to_vec(),
            max_crashes: 3,
            crash_window: Duration::from_secs(5 * 60),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn not_running(plugin: &PluginId, why: impl std::fmt::Display) -> ResponseError {
    ResponseError::new(
        ErrorCode::PluginNotRunning,
        format!("plugin {plugin} is not running: {why}"),
    )
}

fn internal(error: impl std::fmt::Display) -> ResponseError {
    ResponseError::new(ErrorCode::InternalError, error.to_string())
}

/// A started process.
#[derive(Clone)]
struct Process {
    peer: Peer,
    pid: Option<u32>,
    /// `true` asks for SIGKILL, `false` for SIGTERM.
    signal: mpsc::UnboundedSender<bool>,
    exited: watch::Receiver<bool>,
}

/// A started plugin of either kind.
#[derive(Clone)]
enum Conn {
    Process(Process),
    Wasm(Arc<dyn WasmInstance>),
}

/// Why a call into a plugin failed.
enum CallError {
    /// The plugin answered with an error.
    Remote(ResponseError),
    /// The process exited or the instance trapped during the call.
    Gone(String),
    Timeout,
}

impl Conn {
    async fn request_raw(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, CallError> {
        match self {
            Conn::Process(process) => {
                match process.peer.request_raw(method, params, timeout).await {
                    Ok(value) => Ok(value),
                    Err(RpcError::Remote(error)) => Err(CallError::Remote(error)),
                    Err(RpcError::Timeout) => Err(CallError::Timeout),
                    Err(RpcError::ConnectionClosed) => {
                        Err(CallError::Gone("it exited during the call".into()))
                    }
                    Err(error) => Err(CallError::Remote(internal(error))),
                }
            }
            Conn::Wasm(instance) => {
                let call = instance.handle(method.to_owned(), params);
                let result = match timeout {
                    Some(timeout) => tokio::time::timeout(timeout, call)
                        .await
                        .map_err(|_| CallError::Timeout)?,
                    None => call.await,
                };
                match result {
                    Ok(value) => Ok(value),
                    Err(WasmCallError::Rpc(error)) => Err(CallError::Remote(error)),
                    Err(WasmCallError::Trap(trap)) => Err(CallError::Gone(format!("trap: {trap}"))),
                    Err(error @ WasmCallError::Reentrant(_)) => Err(CallError::Remote(
                        ResponseError::new(ErrorCode::InvalidRequest, error.to_string()),
                    )),
                }
            }
        }
    }

    async fn request<R: Request>(
        &self,
        params: &R::Params,
        timeout: Option<Duration>,
    ) -> Result<R::Result, CallError> {
        let params = serde_json::to_value(params).map_err(|e| CallError::Remote(internal(e)))?;
        let result = self.request_raw(R::METHOD, params, timeout).await?;
        serde_json::from_value(result).map_err(|e| CallError::Remote(internal(e)))
    }

    async fn notify<N: Notification>(&self, params: &N::Params) {
        let Ok(params) = serde_json::to_value(params) else {
            return;
        };
        match self {
            Conn::Process(process) => {
                let _ = process.peer.notify_raw(N::METHOD, params).await;
            }
            Conn::Wasm(instance) => {
                let _ = instance.notify(N::METHOD.to_owned(), params).await;
            }
        }
    }

    /// Whether calls can still reach the plugin.
    fn is_alive(&self) -> bool {
        match self {
            Conn::Process(process) => !process.peer.is_closed(),
            Conn::Wasm(instance) => !instance.is_dead(),
        }
    }
}

/// The running plugin and the start it belongs to.
#[derive(Clone)]
struct Running {
    generation: u64,
    conn: Conn,
}

/// A handle for calling a running plugin.
#[derive(Clone)]
pub struct PluginConnection {
    host: PluginHost,
    slot: Arc<Slot>,
    running: Running,
}

impl PluginConnection {
    /// A host-to-plugin request; a crash during the call is recorded right away.
    async fn request<R: Request>(&self, params: &R::Params) -> Result<R::Result, ResponseError> {
        let plugin = self.slot.manifest.id();
        match self.running.conn.request::<R>(params, None).await {
            Ok(result) => Ok(result),
            Err(CallError::Remote(error)) => Err(error),
            Err(CallError::Timeout) => Err(not_running(plugin, "the call timed out")),
            Err(CallError::Gone(why)) => {
                if matches!(self.running.conn, Conn::Wasm(_)) {
                    self.host
                        .exited(&self.slot, self.running.generation, why.clone());
                }
                Err(not_running(plugin, why))
            }
        }
    }
}

struct SlotState {
    state: PluginState,
    reason: Option<String>,
    running: Option<Running>,
    /// Incremented per start, so late notifications about an old start are ignored.
    generation: u64,
    stopping: bool,
    /// Removed from the host (uninstalled, disabled, replaced); never starts again.
    retired: bool,
    crashes: VecDeque<Instant>,
    /// LSP methods the running plugin intercepts (`lspMiddleware` of its `initialize`).
    lsp_middleware: Vec<String>,
}

struct Slot {
    manifest: Manifest,
    dir: PathBuf,
    state: Mutex<SlotState>,
    /// Serializes starts and stops of this plugin.
    lifecycle: tokio::sync::Mutex<()>,
}

impl Slot {
    /// A slot for a ready plugin with code; data plugins have nothing to run.
    fn for_entry(entry: &PluginEntry) -> Option<Arc<Self>> {
        (entry.is_ready() && entry.manifest().plugin.kind != PluginKind::Data).then(|| {
            Arc::new(Self {
                manifest: entry.plugin.manifest.clone(),
                dir: entry.plugin.dir.clone(),
                state: Mutex::new(SlotState {
                    state: PluginState::Enabled,
                    reason: None,
                    running: None,
                    generation: 0,
                    stopping: false,
                    retired: false,
                    crashes: VecDeque::new(),
                    lsp_middleware: Vec::new(),
                }),
                lifecycle: tokio::sync::Mutex::new(()),
            })
        })
    }
}

struct Inner {
    api: Arc<dyn HostApi>,
    events: Arc<dyn EventSink>,
    wasm: Option<Arc<dyn WasmRuntime>>,
    config: SupervisorConfig,
    registry: RwLock<Arc<PluginRegistry>>,
    slots: RwLock<HashMap<PluginId, Arc<Slot>>>,
}

/// Starts, stops and supervises the plugins of a registry.
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<Inner>,
}

impl PluginHost {
    /// Takes over the ready plugins of the registry; nothing starts yet. Without a WASM
    /// runtime, WASM plugins fail to start with a reason.
    pub fn new(
        registry: Arc<PluginRegistry>,
        api: Arc<dyn HostApi>,
        events: Arc<dyn EventSink>,
        wasm: Option<Arc<dyn WasmRuntime>>,
        config: SupervisorConfig,
    ) -> Self {
        let slots = registry
            .entries()
            .filter_map(|entry| Some((entry.manifest().id().clone(), Slot::for_entry(entry)?)))
            .collect();
        Self {
            inner: Arc::new(Inner {
                api,
                events,
                wasm,
                config,
                registry: RwLock::new(registry),
                slots: RwLock::new(slots),
            }),
        }
    }

    /// The registry the host currently works with.
    pub fn registry(&self) -> Arc<PluginRegistry> {
        read(&self.inner.registry).clone()
    }

    /// Switches to a new registry after plugins were installed, removed, enabled or disabled.
    /// Plugins that are no longer ready (or changed) are stopped; new ones can start on demand.
    pub async fn replace_registry(&self, new: Arc<PluginRegistry>) -> RegistryDiff {
        let diff = self.registry().diff(&new);
        for plugin in &diff.removed {
            self.retire(plugin).await;
        }
        {
            let mut slots = write(&self.inner.slots);
            for plugin in &diff.added {
                if let Some(slot) = new.get(plugin).and_then(Slot::for_entry) {
                    slots.insert(plugin.clone(), slot);
                }
            }
        }
        *write(&self.inner.registry) = new;
        diff
    }

    /// Stops the plugin for good and forgets it.
    async fn retire(&self, plugin: &PluginId) {
        self.stop(plugin).await;
        let removed = write(&self.inner.slots).remove(plugin);
        if let Some(slot) = removed {
            let _guard = slot.lifecycle.lock().await;
            lock(&slot.state).retired = true;
        }
    }

    fn slot(&self, plugin: &PluginId) -> Option<Arc<Slot>> {
        read(&self.inner.slots).get(plugin).cloned()
    }

    /// Lifecycle state of a supervised plugin.
    pub fn state(&self, plugin: &PluginId) -> Option<PluginState> {
        self.slot(plugin).map(|slot| lock(&slot.state).state)
    }

    /// Why the plugin is in its state, e.g. the last crash of a Failed plugin.
    pub fn reason(&self, plugin: &PluginId) -> Option<String> {
        self.slot(plugin)
            .and_then(|slot| lock(&slot.state).reason.clone())
    }

    /// Process id of a running process plugin.
    pub fn pid(&self, plugin: &PluginId) -> Option<u32> {
        let slot = self.slot(plugin)?;
        let state = lock(&slot.state);
        match &state.running.as_ref()?.conn {
            Conn::Process(process) => process.pid,
            Conn::Wasm(_) => None,
        }
    }

    /// `service/call` from `caller`: routes to a ready provider of a matching version, starting
    /// it if needed (activation `onInterface`, spec section 14).
    pub async fn call_interface(
        &self,
        caller: &PluginId,
        call: ServiceCallParams,
    ) -> Result<Value, ResponseError> {
        let registry = self.registry();
        let interface = &call.interface;
        let consumes = registry
            .get(caller)
            .and_then(|e| {
                e.manifest()
                    .consumes
                    .iter()
                    .find(|c| &c.interface == interface)
            })
            .ok_or_else(|| {
                ResponseError::new(
                    ErrorCode::PermissionDenied,
                    format!("{caller} does not declare interface {interface} in [[consumes]]"),
                )
            })?;
        let providers = registry.providers(interface);
        if providers.is_empty() {
            return Err(ResponseError::new(
                ErrorCode::NotFound,
                format!("no provider of interface {interface}"),
            ));
        }
        let matching: Vec<&PluginId> =
            providers
                .iter()
                .filter(|provider| {
                    registry.get(provider).is_some_and(|entry| {
                        entry.manifest().provides.iter().any(|p| {
                            &p.interface == interface && consumes.version.matches(&p.version)
                        })
                    })
                })
                .collect();
        let preferred = self.inner.api.preferred_provider(interface);
        let provider = match preferred
            .as_ref()
            .and_then(|p| matching.iter().find(|m| **m == p))
        {
            Some(provider) => *provider,
            None => {
                let Some(first) = matching.first() else {
                    return Err(ResponseError::new(
                        ErrorCode::InterfaceVersionMismatch,
                        format!("no provider of {interface} matches {}", consumes.version),
                    ));
                };
                if matching.len() > 1 {
                    tracing::warn!(%interface, provider = %first, "several providers, taking the first");
                }
                *first
            }
        };
        let connection = self.ensure_running(provider).await?;
        let invoke = ServiceInvokeParams {
            interface: call.interface,
            method: call.method,
            params: call.params,
            caller: caller.clone(),
        };
        connection.request::<ServiceInvoke>(&invoke).await
    }

    /// Runs a command of the plugin, starting the plugin if needed.
    pub async fn invoke_command(
        &self,
        plugin: &PluginId,
        call: CommandCall,
    ) -> Result<Value, ResponseError> {
        let connection = self.ensure_running(plugin).await?;
        connection.request::<InvokeCommand>(&call).await
    }

    /// Asks the plugin how to start one of its language servers (`lsp/resolveLaunch`),
    /// starting the plugin if needed.
    pub async fn resolve_launch(
        &self,
        plugin: &PluginId,
        params: ResolveLaunchParams,
    ) -> Result<LaunchSpec, ResponseError> {
        let connection = self.ensure_running(plugin).await?;
        connection.request::<ResolveLaunch>(&params).await
    }

    /// The plugin's tasks of a provider type (`task/provide`), starting the plugin if needed.
    pub async fn task_provide(
        &self,
        plugin: &PluginId,
        task_type: &str,
    ) -> Result<TaskList, ResponseError> {
        let connection = self.ensure_running(plugin).await?;
        connection
            .request::<TaskProvide>(&TaskProvideParams {
                task_type: task_type.to_owned(),
            })
            .await
    }

    /// Fills in what a task runs (`task/resolve`).
    pub async fn task_resolve(
        &self,
        plugin: &PluginId,
        task: TaskDefinition,
    ) -> Result<TaskDefinition, ResponseError> {
        let connection = self.ensure_running(plugin).await?;
        connection
            .request::<TaskResolve>(&TaskParams { task })
            .await
    }

    /// LSP methods the running plugin intercepts; empty when it does not run.
    pub fn lsp_middleware(&self, plugin: &PluginId) -> Vec<String> {
        self.slot(plugin)
            .map(|slot| {
                let state = lock(&slot.state);
                if state.running.is_some() {
                    state.lsp_middleware.clone()
                } else {
                    Vec::new()
                }
            })
            .unwrap_or_default()
    }

    /// Lets the plugin handle an LSP message it declared in `lspMiddleware`.
    pub async fn lsp_middleware_request(
        &self,
        plugin: &PluginId,
        params: LspMessageParams,
    ) -> Result<MiddlewareResult, ResponseError> {
        let connection = self.ensure_running(plugin).await?;
        connection.request::<LspMiddleware>(&params).await
    }

    /// Starts the plugin unless it runs already; waits for the handshake.
    pub async fn ensure_running(
        &self,
        plugin: &PluginId,
    ) -> Result<PluginConnection, ResponseError> {
        let Some(slot) = self.slot(plugin) else {
            return Err(not_running(plugin, "it is not an enabled plugin with code"));
        };
        if let Some(running) = self.running(&slot).await {
            return Ok(self.connection(&slot, running));
        }
        let _guard = slot.lifecycle.lock().await;
        if let Some(running) = self.running(&slot).await {
            return Ok(self.connection(&slot, running));
        }
        {
            let state = lock(&slot.state);
            if state.retired {
                return Err(not_running(plugin, "it was removed"));
            }
            if state.state == PluginState::Failed {
                let reason = state.reason.clone().unwrap_or_else(|| "failed".into());
                return Err(not_running(plugin, reason));
            }
        }
        let running = self.start(&slot).await?;
        Ok(self.connection(&slot, running))
    }

    fn connection(&self, slot: &Arc<Slot>, running: Running) -> PluginConnection {
        PluginConnection {
            host: self.clone(),
            slot: slot.clone(),
            running,
        }
    }

    /// Stops the plugin gracefully; it may be started again later.
    pub async fn stop(&self, plugin: &PluginId) {
        let Some(slot) = self.slot(plugin) else {
            return;
        };
        let _guard = slot.lifecycle.lock().await;
        let running = {
            let mut state = lock(&slot.state);
            let Some(running) = state.running.clone() else {
                return;
            };
            state.stopping = true;
            running
        };
        self.transition(&slot, PluginState::Stopping, None);
        let config = &self.inner.config;
        let conn = &running.conn;
        let _ = conn
            .request::<Shutdown>(&(), Some(config.shutdown_timeout))
            .await;
        conn.notify::<Exit>(&()).await;
        match conn {
            Conn::Process(process) => {
                let mut exited = process.exited.clone();
                for force in [None, Some(false), Some(true)] {
                    if let Some(force) = force {
                        let _ = process.signal.send(force);
                    }
                    if wait_exit(&mut exited, config.terminate_timeout).await {
                        break;
                    }
                }
            }
            // Dropping the instance is all there is to stopping a component.
            Conn::Wasm(_) => self.exited(&slot, running.generation, "stopped".into()),
        }
    }

    /// Stops every running plugin, e.g. when the IDE exits.
    pub async fn stop_all(&self) {
        let mut tasks = tokio::task::JoinSet::new();
        let plugins: Vec<PluginId> = read(&self.inner.slots).keys().cloned().collect();
        for plugin in plugins {
            let host = self.clone();
            tasks.spawn(async move { host.stop(&plugin).await });
        }
        while tasks.join_next().await.is_some() {}
    }

    /// Starts and initializes the plugin. Called with the lifecycle lock held.
    async fn start(&self, slot: &Arc<Slot>) -> Result<Running, ResponseError> {
        let plugin = slot.manifest.id().clone();
        self.transition(slot, PluginState::Starting, None);
        let generation = {
            let mut state = lock(&slot.state);
            state.generation += 1;
            state.stopping = false;
            state.generation
        };
        let (conn, timeout) = match slot.manifest.plugin.kind {
            PluginKind::Wasm => (
                self.instantiate(slot).await,
                self.inner.config.wasm_start_timeout,
            ),
            PluginKind::Process | PluginKind::Data => (
                self.spawn(slot, generation).await,
                self.inner.config.start_timeout,
            ),
        };
        let conn = match conn {
            Ok(conn) => conn,
            Err(error) => return Err(self.fail_start(slot, error)),
        };
        let running = Running { generation, conn };
        lock(&slot.state).running = Some(running.clone());

        let session = self.inner.api.session(&slot.manifest);
        let params = InitializeParams {
            api_version: protocol_version().to_string(),
            plugin_id: plugin.clone(),
            host_capabilities: HostCapabilities {
                methods: vec![
                    ExecuteCommand::METHOD.into(),
                    SetContext::METHOD.into(),
                    ServiceCall::METHOD.into(),
                    LspSendRequest::METHOD.into(),
                    LspApplyEdit::METHOD.into(),
                ],
                experimental: None,
            },
            workspace: session.workspace,
            config: session.config,
            storage: session.storage,
            locale: session.locale,
        };
        let conn = &running.conn;
        let handshake = match conn.request::<Initialize>(&params, Some(timeout)).await {
            Ok(result) => {
                lock(&slot.state).lsp_middleware = result.plugin_capabilities.lsp_middleware;
                conn.notify::<Initialized>(&()).await;
                Ok(())
            }
            Err(CallError::Timeout) => Err("no answer to initialize in time".to_owned()),
            Err(CallError::Remote(error)) => Err(format!("initialize failed: {}", error.message)),
            Err(CallError::Gone(why)) => Err(format!("initialize failed: {why}")),
        };
        if let Err(reason) = handshake {
            lock(&slot.state).stopping = true;
            if let Conn::Process(process) = conn {
                let _ = process.signal.send(true);
            }
            return Err(self.fail_start(slot, reason));
        }
        self.transition(slot, PluginState::Running, None);
        Ok(running)
    }

    fn fail_start(&self, slot: &Slot, reason: String) -> ResponseError {
        tracing::warn!(plugin = %slot.manifest.id(), "start failed: {reason}");
        lock(&slot.state).running = None;
        self.transition(slot, PluginState::Failed, Some(reason.clone()));
        not_running(slot.manifest.id(), reason)
    }

    fn bridge(&self, slot: &Slot) -> Bridge {
        Bridge {
            manifest: slot.manifest.clone(),
            api: self.inner.api.clone(),
            // Weak: the plugin's connection must not keep the host alive.
            host: Arc::downgrade(&self.inner),
        }
    }

    async fn instantiate(&self, slot: &Arc<Slot>) -> Result<Conn, String> {
        let runtime = self
            .inner
            .wasm
            .clone()
            .ok_or_else(|| "no WASM runtime is available".to_owned())?;
        let manifest = &slot.manifest;
        let entry = manifest
            .plugin
            .entry
            .as_ref()
            .ok_or_else(|| "the manifest has no entry".to_owned())?;
        let spec = WasmInstanceSpec {
            plugin: manifest.id().clone(),
            component: entry.resolve(&slot.dir),
            memory_limit: manifest.plugin.memory.map(|mib| {
                usize::try_from(mib)
                    .unwrap_or(usize::MAX)
                    .saturating_mul(1 << 20)
            }),
        };
        let bridge: Arc<dyn WasmHost> = Arc::new(self.bridge(slot));
        let instance = runtime.instantiate(spec, bridge).await?;
        Ok(Conn::Wasm(instance))
    }

    async fn spawn(&self, slot: &Arc<Slot>, generation: u64) -> Result<Conn, String> {
        let plugin = slot.manifest.id().clone();
        let entry = slot
            .manifest
            .entry_file(&slot.dir)
            .ok_or_else(|| "the manifest has no entry".to_owned())?;
        let mut command = Command::new(&entry);
        command
            .current_dir(&slot.dir)
            .env("KUMIKO_PLUGIN_ID", plugin.as_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut attempt = 0;
        let mut child = loop {
            match command.spawn() {
                Ok(child) => break child,
                // A freshly installed executable can be briefly "busy": a child forked by another
                // thread while the file was being written holds the write handle until its exec.
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 10 =>
                {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(10 * attempt)).await;
                }
                Err(error) => return Err(format!("cannot start {}: {error}", entry.display())),
            }
        };
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err("the process has no stdio pipes".into());
        };
        let pid = child.id();
        let tree = ProcessTree::new(&child, &plugin);

        let log_plugin = plugin.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(plugin = %log_plugin, "{line}");
            }
        });

        let bridge = self.bridge(slot);
        let peer = Peer::spawn(stdout, stdin, PeerConfig::default(), move |_| bridge);
        let (signal, signals) = mpsc::unbounded_channel();
        let (exited_tx, exited) = watch::channel(false);
        let host = self.clone();
        let monitored = slot.clone();
        let monitor_peer = peer.clone();
        tokio::spawn(async move {
            let status = monitor(child, tree, signals).await;
            monitor_peer.close();
            host.exited(&monitored, generation, status);
            // After the state change, so a stop() that waits for this sees the final state.
            exited_tx.send_replace(true);
        });
        Ok(Conn::Process(Process {
            peer,
            pid,
            signal,
            exited,
        }))
    }

    /// The start `generation` is gone (process exit, trap, stop): a stop, or a crash to
    /// recover from. Only the first report of a start counts.
    fn exited(&self, slot: &Arc<Slot>, generation: u64, status: String) {
        let (crashed, restart) = {
            let mut state = lock(&slot.state);
            let current = state
                .running
                .as_ref()
                .is_some_and(|r| r.generation == generation);
            if state.generation != generation || !current {
                return;
            }
            state.running = None;
            if state.stopping || state.state == PluginState::Failed {
                (false, None)
            } else {
                let now = Instant::now();
                let window = self.inner.config.crash_window;
                state.crashes.push_back(now);
                while state
                    .crashes
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > window)
                {
                    state.crashes.pop_front();
                }
                let count = state.crashes.len();
                if count >= self.inner.config.max_crashes {
                    (true, None)
                } else {
                    let backoff = &self.inner.config.backoff;
                    let delay = backoff
                        .get(count - 1)
                        .or(backoff.last())
                        .copied()
                        .unwrap_or_default();
                    (true, Some(delay))
                }
            }
        };
        let plugin = slot.manifest.id().clone();
        if !crashed {
            // A requested stop; a failed start keeps its Failed state and reason.
            if lock(&slot.state).state != PluginState::Failed {
                self.transition(slot, PluginState::Enabled, None);
            }
            return;
        }
        tracing::warn!(%plugin, "plugin crashed: {status}");
        let Some(delay) = restart else {
            let reason = format!(
                "crashed {} times within {} s, last: {status}",
                self.inner.config.max_crashes,
                self.inner.config.crash_window.as_secs()
            );
            self.transition(slot, PluginState::Failed, Some(reason));
            return;
        };
        self.transition(slot, PluginState::Crashed, Some(status));
        let host = self.clone();
        let slot = slot.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _guard = slot.lifecycle.lock().await;
            let still_crashed = {
                let state = lock(&slot.state);
                state.generation == generation
                    && state.state == PluginState::Crashed
                    && !state.retired
            };
            if still_crashed && let Err(error) = host.start(&slot).await {
                tracing::warn!(plugin = %slot.manifest.id(), "restart failed: {}", error.message);
            }
        });
    }

    fn transition(&self, slot: &Slot, state: PluginState, reason: Option<String>) {
        {
            let mut current = lock(&slot.state);
            current.state = state;
            current.reason = reason.clone();
        }
        self.inner.api.state_changed(&slot.manifest, state);
        let plugin = slot.manifest.id().clone();
        self.inner.events.publish(Event::core(&PluginStateChanged {
            plugin,
            state,
            reason,
        }));
    }

    /// The running plugin, if calls can reach it. A lost connection or a trapped instance
    /// means the plugin is gone: a dying process is killed, and the exit is recorded (as a
    /// crash) before the caller decides to start it again.
    async fn running(&self, slot: &Arc<Slot>) -> Option<Running> {
        let running = {
            let state = lock(&slot.state);
            match (&state.state, &state.running) {
                (PluginState::Running, Some(running)) => running.clone(),
                _ => return None,
            }
        };
        if running.conn.is_alive() {
            return Some(running);
        }
        match &running.conn {
            Conn::Process(process) => {
                let _ = process.signal.send(true);
                let mut exited = process.exited.clone();
                wait_exit(&mut exited, self.inner.config.terminate_timeout).await;
            }
            Conn::Wasm(_) => self.exited(slot, running.generation, "trapped".into()),
        }
        None
    }
}

async fn wait_exit(exited: &mut watch::Receiver<bool>, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, exited.wait_for(|e| *e)).await,
        Ok(Ok(_))
    )
}

/// Waits for the process, delivering signals asked for meanwhile; returns its exit status.
/// Descendants do not outlive the plugin: once it exits, what is left of its tree is killed.
async fn monitor(
    mut child: Child,
    tree: ProcessTree,
    mut signals: mpsc::UnboundedReceiver<bool>,
) -> String {
    loop {
        tokio::select! {
            status = child.wait() => {
                // On Unix the group id stays reserved while any member lives, so this reaches
                // only the plugin's descendants (or nobody).
                tree.signal(true);
                return match status {
                    Ok(status) => status.to_string(),
                    Err(error) => format!("wait failed: {error}"),
                };
            }
            Some(force) = signals.recv() => {
                tree.signal(force);
                if force {
                    let _ = child.start_kill();
                }
            }
        }
    }
}

/// A process plugin and its descendants: the process group it leads on Unix, a Job Object
/// on Windows. The only place where stopping a plugin depends on the OS.
struct ProcessTree {
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
    #[cfg(windows)]
    job: Option<crate::job::Job>,
}

impl ProcessTree {
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn new(child: &Child, plugin: &PluginId) -> Self {
        Self {
            #[cfg(unix)]
            group: child
                .id()
                .and_then(|p| i32::try_from(p).ok())
                .and_then(rustix::process::Pid::from_raw),
            #[cfg(windows)]
            job: child
                .raw_handle()
                .and_then(|process| match crate::job::Job::assign(process) {
                    Ok(job) => Some(job),
                    Err(error) => {
                        tracing::warn!(%plugin, "no job object, children may outlive stops: {error}");
                        None
                    }
                }),
        }
    }

    /// Asks the tree to terminate (SIGTERM; Windows has no such request, the stop sequence's
    /// `exit` notification plays its part) or kills it (SIGKILL, `TerminateJobObject`).
    #[cfg_attr(not(any(unix, windows)), allow(unused_variables))]
    fn signal(&self, force: bool) {
        #[cfg(unix)]
        if let Some(group) = self.group {
            use rustix::process::{Signal, kill_process_group};
            let signal = if force { Signal::KILL } else { Signal::TERM };
            let _ = kill_process_group(group, signal);
        }
        #[cfg(windows)]
        if force && let Some(job) = &self.job {
            job.terminate();
        }
    }
}

/// The permission a plugin-to-host method needs (spec, section 13).
fn required_permission(method: &str) -> Option<Permission> {
    if method == "task/run" {
        return Some(Permission::ProcessSpawn);
    }
    match method.split('/').next() {
        Some("fs" | "document") => Some(Permission::FsWorkspace),
        Some("process") => Some(Permission::ProcessSpawn),
        Some("ui") => Some(Permission::Ui),
        _ => None,
    }
}

/// Plugin-to-host requests of one plugin, for both kinds.
struct Bridge {
    manifest: Manifest,
    api: Arc<dyn HostApi>,
    host: Weak<Inner>,
}

fn params<T: DeserializeOwned>(method: &str, params: Value) -> Result<T, ResponseError> {
    serde_json::from_value(params).map_err(|error| {
        ResponseError::new(
            ErrorCode::InvalidParams,
            format!("invalid params for `{method}`: {error}"),
        )
    })
}

fn result(value: impl Serialize) -> Result<Value, ResponseError> {
    serde_json::to_value(value).map_err(internal)
}

impl Bridge {
    /// Dispatches a request. `enforce` applies the manifest's permissions (WASM plugins).
    fn call(
        &self,
        method: &str,
        request: Value,
        enforce: bool,
    ) -> BoxFuture<'static, Result<Value, ResponseError>> {
        let manifest = &self.manifest;
        let plugin = manifest.id().clone();
        if enforce
            && let Some(permission) = required_permission(method)
            && !manifest.has_permission(permission)
        {
            let error = ResponseError::new(
                ErrorCode::PermissionDenied,
                format!("{plugin} has no permission `{permission}` for `{method}`"),
            );
            return Box::pin(std::future::ready(Err(error)));
        }
        let ready = |result| -> BoxFuture<'static, _> { Box::pin(std::future::ready(result)) };
        match method {
            ExecuteCommand::METHOD => match params(method, request) {
                Ok(call) => self.api.execute_command(&plugin, call),
                Err(error) => ready(Err(error)),
            },
            SetContext::METHOD => ready(
                params::<kumiko_protocol::methods::command::SetContextParams>(method, request)
                    .and_then(|params| {
                        if !manifest.owns_key(&params.key) {
                            return Err(ResponseError::new(
                                ErrorCode::PermissionDenied,
                                format!(
                                    "context key `{}` is outside the plugin namespace `{}.*`",
                                    params.key,
                                    manifest.namespace()
                                ),
                            ));
                        }
                        self.api.set_context(params.key, params.value);
                        result(())
                    }),
            ),
            LspApplyEdit::METHOD => match params(method, request) {
                Ok(call) => {
                    let applied = self.api.lsp_apply_edit(&plugin, call);
                    Box::pin(async move { applied.await.and_then(result) })
                }
                Err(error) => ready(Err(error)),
            },
            LspSendRequest::METHOD => match params(method, request) {
                Ok(call) => self.api.lsp_send_request(&plugin, call),
                Err(error) => ready(Err(error)),
            },
            ServiceCall::METHOD => {
                let call = match params(method, request) {
                    Ok(call) => call,
                    Err(error) => return ready(Err(error)),
                };
                let host = self.host.upgrade().map(|inner| PluginHost { inner });
                Box::pin(async move {
                    match host {
                        Some(host) => host.call_interface(&plugin, call).await,
                        None => Err(internal("the host is gone")),
                    }
                })
            }
            _ => ready(Err(ResponseError::new(
                ErrorCode::MethodNotFound,
                format!("the host does not implement `{method}` yet"),
            ))),
        }
    }
}

/// Process plugins: requests arrive over KPP frames.
impl Handler for Bridge {
    fn request(&self, method: String, params: Value) -> HandlerFuture {
        self.call(&method, params, false)
    }
}

/// WASM plugins: requests arrive through the `host` import.
impl WasmHost for Bridge {
    fn call(
        &self,
        method: String,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, ResponseError>> {
        Bridge::call(self, &method, params, true)
    }

    fn notify(&self, method: String, _params: Value) {
        tracing::debug!(plugin = %self.manifest.id(), %method, "ignoring a plugin notification");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_map_to_permissions() {
        assert_eq!(
            required_permission("fs/read"),
            Some(Permission::FsWorkspace)
        );
        assert_eq!(
            required_permission("document/applyEdit"),
            Some(Permission::FsWorkspace)
        );
        assert_eq!(
            required_permission("process/spawn"),
            Some(Permission::ProcessSpawn)
        );
        assert_eq!(
            required_permission("task/run"),
            Some(Permission::ProcessSpawn)
        );
        assert_eq!(required_permission("task/provide"), None);
        assert_eq!(required_permission("ui/showMessage"), Some(Permission::Ui));
        assert_eq!(required_permission("command/execute"), None);
        assert_eq!(required_permission("service/call"), None);
    }
}
