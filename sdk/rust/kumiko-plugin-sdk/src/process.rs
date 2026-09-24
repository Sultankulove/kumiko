//! Process plugins: a binary that speaks KPP with the host over stdin/stdout (spec, section 16).
//!
//! ```no_run
//! use kumiko_plugin_sdk::process::Plugin;
//! use serde_json::json;
//!
//! # async fn run() -> Result<(), kumiko_plugin_sdk::process::SdkError> {
//! Plugin::new()
//!     .command("hello.say", |_ctx, args| async move { Ok(json!({ "said": args })) })
//!     .run_stdio()
//!     .await
//! # }
//! ```
//!
//! Stdout carries protocol frames, so a plugin must log to stderr only.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;

use kumiko_ipc::{Peer, PeerConfig, Router, RpcError};
use kumiko_protocol::PROTOCOL_VERSION;
use kumiko_protocol::common::CommandCall;
use kumiko_protocol::methods::command::{
    ExecuteCommand, InvokeCommand, SetContext, SetContextParams,
};
use kumiko_protocol::methods::lifecycle::{
    Exit, Initialize, InitializeParams, InitializeResult, Initialized, PluginCapabilities, Shutdown,
};
use kumiko_protocol::{ErrorCode, ResponseError};
use kumiko_types::{CommandId, PluginId};

type CommandFuture = Pin<Box<dyn Future<Output = Result<Value, ResponseError>> + Send>>;
type CommandFn = Arc<dyn Fn(Context, Vec<Value>) -> CommandFuture + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// The host went away without `exit`.
    #[error("connection to the host closed")]
    ConnectionClosed,
}

/// What a command handler gets: the handshake data and a way to call the host.
#[derive(Clone)]
pub struct Context {
    peer: Peer,
    init: Arc<InitializeParams>,
}

impl Context {
    pub fn plugin_id(&self) -> &PluginId {
        &self.init.plugin_id
    }

    /// Everything the host sent in `initialize`: workspace, settings, locale, capabilities.
    pub fn params(&self) -> &InitializeParams {
        &self.init
    }

    /// The raw connection, for methods without a helper here.
    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    /// Runs any command: of the core, of this plugin or of another plugin.
    pub async fn execute_command(
        &self,
        command: CommandId,
        args: Vec<Value>,
    ) -> Result<Value, RpcError> {
        self.peer
            .request::<ExecuteCommand>(&CommandCall { command, args })
            .await
    }

    /// Sets a context key; the host accepts keys in the plugin's own namespace only.
    pub async fn set_context(&self, key: impl Into<String>, value: Value) -> Result<(), RpcError> {
        self.peer
            .request::<SetContext>(&SetContextParams {
                key: key.into(),
                value,
            })
            .await
    }
}

/// A process plugin under construction.
#[derive(Default)]
pub struct Plugin {
    commands: HashMap<String, CommandFn>,
    capabilities: PluginCapabilities,
}

struct State {
    init: Mutex<Option<Arc<InitializeParams>>>,
    exit: Notify,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Plugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles a command contributed in the manifest (or registered at runtime).
    pub fn command<F, Fut>(mut self, id: &str, handler: F) -> Self
    where
        F: Fn(Context, Vec<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, ResponseError>> + Send + 'static,
    {
        let handler: CommandFn = Arc::new(move |ctx, args| Box::pin(handler(ctx, args)));
        self.commands.insert(id.to_owned(), handler);
        self
    }

    /// Declares optional host-to-plugin methods the plugin handles.
    pub fn capabilities(mut self, capabilities: PluginCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Serves the host over stdin/stdout until `exit`.
    pub async fn run_stdio(self) -> Result<(), SdkError> {
        self.serve(tokio::io::stdin(), tokio::io::stdout()).await
    }

    /// Serves the host over any byte stream until `exit` or until the connection closes.
    pub async fn serve<R, W>(self, reader: R, writer: W) -> Result<(), SdkError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let state = Arc::new(State {
            init: Mutex::new(None),
            exit: Notify::new(),
        });
        let commands = Arc::new(self.commands);
        let capabilities = self.capabilities;
        let router_state = state.clone();
        let peer = Peer::spawn(reader, writer, PeerConfig::default(), move |peer| {
            router(peer, router_state, commands, capabilities)
        });
        tokio::select! {
            () = state.exit.notified() => Ok(()),
            () = peer.closed() => Err(SdkError::ConnectionClosed),
        }
    }
}

fn router(
    peer: Peer,
    state: Arc<State>,
    commands: Arc<HashMap<String, CommandFn>>,
    capabilities: PluginCapabilities,
) -> Router {
    let init_state = state.clone();
    let invoke_state = state.clone();
    Router::new()
        .on_request::<Initialize, _, _>(move |params| {
            let result = if compatible(&params.api_version, PROTOCOL_VERSION) {
                *lock(&init_state.init) = Some(Arc::new(params));
                Ok(InitializeResult {
                    api_version: PROTOCOL_VERSION.to_owned(),
                    plugin_capabilities: capabilities.clone(),
                })
            } else {
                Err(ResponseError::new(
                    ErrorCode::ApiVersionMismatch,
                    format!(
                        "the plugin speaks KPP {PROTOCOL_VERSION}, the host {}",
                        params.api_version
                    ),
                ))
            };
            std::future::ready(result)
        })
        .on_notification::<Initialized, _>(|()| {})
        .on_request::<InvokeCommand, _, _>(move |call: CommandCall| {
            let init = lock(&invoke_state.init).clone();
            let handler = commands.get(call.command.as_str()).cloned();
            let peer = peer.clone();
            async move {
                let Some(init) = init else {
                    return Err(ResponseError::new(
                        ErrorCode::InvalidRequest,
                        "not initialized",
                    ));
                };
                let Some(handler) = handler else {
                    return Err(ResponseError::new(
                        ErrorCode::NotFound,
                        format!("command `{}` is not handled by this plugin", call.command),
                    ));
                };
                handler(Context { peer, init }, call.args).await
            }
        })
        .on_request::<Shutdown, _, _>(|()| std::future::ready(Ok(())))
        .on_notification::<Exit, _>(move |()| state.exit.notify_one())
}

/// Same major version; in 0.x the minor version must match too (semver caret rules).
fn compatible(host: &str, plugin: &str) -> bool {
    let parts = |v: &str| {
        let mut numbers = v.split('.').map(|n| n.parse::<u64>().ok());
        (numbers.next().flatten(), numbers.next().flatten())
    };
    match (parts(host), parts(plugin)) {
        ((Some(0), Some(a)), (Some(0), Some(b))) => a == b,
        ((Some(a), Some(_)), (Some(b), Some(_))) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::compatible;

    #[test]
    fn version_compatibility() {
        assert!(compatible("0.1.0", "0.1.7"));
        assert!(!compatible("0.2.0", "0.1.0"));
        assert!(compatible("1.4.0", "1.0.0"));
        assert!(!compatible("2.0.0", "1.9.0"));
        assert!(!compatible("one", "0.1.0"));
    }
}
