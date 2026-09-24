//! Running process plugins: activation, the KPP bridge and supervision (spec, section 13).
//!
//! A plugin process is started on first use and initialized with a timeout. Its exit without a
//! stop request is a crash: the host releases what the plugin registered at runtime and restarts
//! it with backoff; after too many crashes in a window the plugin is Failed until the IDE
//! restarts. Stopping goes `shutdown` → `exit` → SIGTERM → SIGKILL, and each plugin runs in its
//! own process group so its children go down with it.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};

use kumiko_ipc::{Peer, PeerConfig, Router, RpcError};
use kumiko_protocol::common::CommandCall;
use kumiko_protocol::events::{PluginState, PluginStateChanged};
use kumiko_protocol::methods::command::{ExecuteCommand, InvokeCommand, SetContext};
use kumiko_protocol::methods::lifecycle::{
    Exit, HostCapabilities, Initialize, InitializeParams, Initialized, Shutdown, WorkspaceInfo,
};
use kumiko_protocol::{ErrorCode, Request, ResponseError};
use kumiko_service_api::{BoxFuture, Event, EventSink};
use kumiko_types::PluginId;

use crate::manifest::{Manifest, PluginKind};
use crate::registry::{PluginRegistry, protocol_version};

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
}

/// Per-plugin part of `initialize`.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub workspace: WorkspaceInfo,
    /// Current values of the plugin's own settings.
    pub config: Value,
    pub locale: String,
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// From spawn to the `initialize` result.
    pub start_timeout: Duration,
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

fn not_running(plugin: &PluginId, why: impl std::fmt::Display) -> ResponseError {
    ResponseError::new(
        ErrorCode::PluginNotRunning,
        format!("plugin {plugin} is not running: {why}"),
    )
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

struct SlotState {
    state: PluginState,
    reason: Option<String>,
    process: Option<Process>,
    /// Incremented per start, so late notifications about an old process are ignored.
    generation: u64,
    stopping: bool,
    crashes: VecDeque<Instant>,
}

struct Slot {
    manifest: Manifest,
    dir: PathBuf,
    state: Mutex<SlotState>,
    /// Serializes starts and stops of this plugin.
    lifecycle: tokio::sync::Mutex<()>,
}

struct Inner {
    api: Arc<dyn HostApi>,
    events: Arc<dyn EventSink>,
    config: SupervisorConfig,
    slots: HashMap<PluginId, Arc<Slot>>,
}

/// Starts, stops and supervises the process plugins of a registry.
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<Inner>,
}

impl PluginHost {
    /// Takes over the ready process plugins of the registry; nothing starts yet.
    pub fn new(
        registry: &PluginRegistry,
        api: Arc<dyn HostApi>,
        events: Arc<dyn EventSink>,
        config: SupervisorConfig,
    ) -> Self {
        let slots = registry
            .ready()
            .filter(|entry| entry.manifest().plugin.kind == PluginKind::Process)
            .map(|entry| {
                let slot = Slot {
                    manifest: entry.plugin.manifest.clone(),
                    dir: entry.plugin.dir.clone(),
                    state: Mutex::new(SlotState {
                        state: PluginState::Enabled,
                        reason: None,
                        process: None,
                        generation: 0,
                        stopping: false,
                        crashes: VecDeque::new(),
                    }),
                    lifecycle: tokio::sync::Mutex::new(()),
                };
                (entry.manifest().id().clone(), Arc::new(slot))
            })
            .collect();
        Self {
            inner: Arc::new(Inner {
                api,
                events,
                config,
                slots,
            }),
        }
    }

    /// Lifecycle state of a supervised plugin.
    pub fn state(&self, plugin: &PluginId) -> Option<PluginState> {
        self.inner
            .slots
            .get(plugin)
            .map(|slot| lock(&slot.state).state)
    }

    /// Process id of a running plugin.
    pub fn pid(&self, plugin: &PluginId) -> Option<u32> {
        let slot = self.inner.slots.get(plugin)?;
        lock(&slot.state).process.as_ref().and_then(|p| p.pid)
    }

    /// Runs a command of the plugin, starting the plugin if needed.
    pub async fn invoke_command(
        &self,
        plugin: &PluginId,
        call: CommandCall,
    ) -> Result<Value, ResponseError> {
        let peer = self.ensure_running(plugin).await?;
        match peer.request::<InvokeCommand>(&call).await {
            Ok(value) => Ok(value),
            Err(RpcError::Remote(error)) => Err(error),
            Err(RpcError::ConnectionClosed) => {
                Err(not_running(plugin, "it exited during the call"))
            }
            Err(error) => Err(ResponseError::new(
                ErrorCode::InternalError,
                error.to_string(),
            )),
        }
    }

    /// Starts the plugin unless it runs already; waits for the handshake.
    pub async fn ensure_running(&self, plugin: &PluginId) -> Result<Peer, ResponseError> {
        let Some(slot) = self.inner.slots.get(plugin).cloned() else {
            return Err(not_running(plugin, "it is not an enabled process plugin"));
        };
        if let Some(peer) = self.running_peer(&slot).await {
            return Ok(peer);
        }
        let _guard = slot.lifecycle.lock().await;
        if let Some(peer) = self.running_peer(&slot).await {
            return Ok(peer);
        }
        {
            let state = lock(&slot.state);
            if state.state == PluginState::Failed {
                let reason = state.reason.clone().unwrap_or_else(|| "failed".into());
                return Err(not_running(plugin, reason));
            }
        }
        self.start(&slot).await
    }

    /// Stops the plugin gracefully; it may be started again later.
    pub async fn stop(&self, plugin: &PluginId) {
        let Some(slot) = self.inner.slots.get(plugin).cloned() else {
            return;
        };
        let _guard = slot.lifecycle.lock().await;
        let process = {
            let mut state = lock(&slot.state);
            let Some(process) = state.process.clone() else {
                return;
            };
            state.stopping = true;
            process
        };
        self.transition(&slot, PluginState::Stopping, None);
        let config = &self.inner.config;
        let _ = process
            .peer
            .request_timeout::<Shutdown>(&(), Some(config.shutdown_timeout))
            .await;
        let _ = process.peer.notify::<Exit>(&()).await;
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

    /// Stops every running plugin, e.g. when the IDE exits.
    pub async fn stop_all(&self) {
        let mut tasks = tokio::task::JoinSet::new();
        let plugins: Vec<PluginId> = self.inner.slots.keys().cloned().collect();
        for plugin in plugins {
            let host = self.clone();
            tasks.spawn(async move { host.stop(&plugin).await });
        }
        while tasks.join_next().await.is_some() {}
    }

    /// Spawns and initializes the process. Called with the lifecycle lock held.
    async fn start(&self, slot: &Arc<Slot>) -> Result<Peer, ResponseError> {
        let plugin = slot.manifest.id().clone();
        self.transition(slot, PluginState::Starting, None);
        let generation = {
            let mut state = lock(&slot.state);
            state.generation += 1;
            state.stopping = false;
            state.generation
        };
        let process = match self.spawn(slot, generation) {
            Ok(process) => process,
            Err(error) => return Err(self.fail_start(slot, error)),
        };
        lock(&slot.state).process = Some(process.clone());

        let session = self.inner.api.session(&slot.manifest);
        let params = InitializeParams {
            api_version: protocol_version().to_string(),
            plugin_id: plugin.clone(),
            host_capabilities: HostCapabilities {
                methods: vec![ExecuteCommand::METHOD.into(), SetContext::METHOD.into()],
                experimental: None,
            },
            workspace: session.workspace,
            config: session.config,
            locale: session.locale,
        };
        let timeout = Some(self.inner.config.start_timeout);
        let handshake = match process
            .peer
            .request_timeout::<Initialize>(&params, timeout)
            .await
        {
            Ok(_) => process
                .peer
                .notify::<Initialized>(&())
                .await
                .map_err(|e| e.to_string()),
            Err(RpcError::Timeout) => Err("no answer to initialize in time".to_owned()),
            Err(error) => Err(format!("initialize failed: {error}")),
        };
        if let Err(reason) = handshake {
            {
                let mut state = lock(&slot.state);
                state.stopping = true;
            }
            let _ = process.signal.send(true);
            return Err(self.fail_start(slot, reason));
        }
        self.transition(slot, PluginState::Running, None);
        Ok(process.peer)
    }

    fn fail_start(&self, slot: &Slot, reason: String) -> ResponseError {
        tracing::warn!(plugin = %slot.manifest.id(), "start failed: {reason}");
        lock(&slot.state).process = None;
        self.transition(slot, PluginState::Failed, Some(reason.clone()));
        not_running(slot.manifest.id(), reason)
    }

    fn spawn(&self, slot: &Arc<Slot>, generation: u64) -> Result<Process, String> {
        let plugin = slot.manifest.id().clone();
        let entry = slot
            .manifest
            .plugin
            .entry
            .as_ref()
            .ok_or_else(|| "the manifest has no entry".to_owned())?
            .resolve(&slot.dir);
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
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot start {}: {error}", entry.display()))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err("the process has no stdio pipes".into());
        };
        let pid = child.id();

        let log_plugin = plugin.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(plugin = %log_plugin, "{line}");
            }
        });

        let bridge = Bridge {
            plugin: plugin.clone(),
            manifest: slot.manifest.clone(),
            api: self.inner.api.clone(),
        };
        let peer = Peer::spawn(stdout, stdin, PeerConfig::default(), move |_| {
            bridge.router()
        });
        let (signal, signals) = mpsc::unbounded_channel();
        let (exited_tx, exited) = watch::channel(false);
        let host = self.clone();
        let monitored = slot.clone();
        let monitor_peer = peer.clone();
        tokio::spawn(async move {
            let status = monitor(child, pid, signals).await;
            monitor_peer.close();
            host.exited(&monitored, generation, status);
            // After the state change, so a stop() that waits for this sees the final state.
            exited_tx.send_replace(true);
        });
        Ok(Process {
            peer,
            pid,
            signal,
            exited,
        })
    }

    /// The process of `generation` is gone: a stop, or a crash to recover from.
    fn exited(&self, slot: &Arc<Slot>, generation: u64, status: String) {
        let (crashed, restart) = {
            let mut state = lock(&slot.state);
            if state.generation != generation {
                return;
            }
            state.process = None;
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
                state.generation == generation && state.state == PluginState::Crashed
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
}

impl PluginHost {
    /// The connection of a running plugin. A lost connection means a dying process: it is
    /// killed and its exit recorded (as a crash) before the caller decides to start it again.
    async fn running_peer(&self, slot: &Slot) -> Option<Peer> {
        let process = {
            let state = lock(&slot.state);
            match (&state.state, &state.process) {
                (PluginState::Running, Some(process)) => process.clone(),
                _ => return None,
            }
        };
        if !process.peer.is_closed() {
            return Some(process.peer);
        }
        let _ = process.signal.send(true);
        let mut exited = process.exited.clone();
        wait_exit(&mut exited, self.inner.config.terminate_timeout).await;
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
async fn monitor(
    mut child: Child,
    pid: Option<u32>,
    mut signals: mpsc::UnboundedReceiver<bool>,
) -> String {
    loop {
        tokio::select! {
            status = child.wait() => {
                return match status {
                    Ok(status) => status.to_string(),
                    Err(error) => format!("wait failed: {error}"),
                };
            }
            Some(force) = signals.recv() => {
                signal_group(pid, force);
                if force {
                    let _ = child.start_kill();
                }
            }
        }
    }
}

/// SIGTERM or SIGKILL to the plugin's process group.
#[cfg(unix)]
fn signal_group(pid: Option<u32>, force: bool) {
    use rustix::process::{Pid, Signal, kill_process_group};
    let Some(pid) = pid
        .and_then(|p| i32::try_from(p).ok())
        .and_then(Pid::from_raw)
    else {
        return;
    };
    let signal = if force { Signal::KILL } else { Signal::TERM };
    let _ = kill_process_group(pid, signal);
}

#[cfg(not(unix))]
fn signal_group(_pid: Option<u32>, _force: bool) {}

/// Plugin-to-host methods of one plugin.
struct Bridge {
    plugin: PluginId,
    manifest: Manifest,
    api: Arc<dyn HostApi>,
}

impl Bridge {
    fn router(self) -> Router {
        let Bridge {
            plugin,
            manifest,
            api,
        } = self;
        let execute_api = api.clone();
        Router::new()
            .on_request::<ExecuteCommand, _, _>(move |call| {
                execute_api.execute_command(&plugin, call)
            })
            .on_request::<SetContext, _, _>(move |params| {
                let result = if manifest.owns_key(&params.key) {
                    api.set_context(params.key, params.value);
                    Ok(())
                } else {
                    Err(ResponseError::new(
                        ErrorCode::PermissionDenied,
                        format!(
                            "context key `{}` is outside the plugin namespace `{}.*`",
                            params.key,
                            manifest.namespace()
                        ),
                    ))
                };
                std::future::ready(result)
            })
    }
}
