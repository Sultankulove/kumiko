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
use kumiko_protocol::methods::service::{ServiceCall, ServiceCallParams, ServiceInvoke};
use kumiko_protocol::{ErrorCode, ResponseError};
use kumiko_types::{CommandId, InterfaceId, PluginId};

type CommandFuture = Pin<Box<dyn Future<Output = Result<Value, ResponseError>> + Send>>;
type CommandFn = Arc<dyn Fn(Context, Vec<Value>) -> CommandFuture + Send + Sync>;
type InterfaceFn = Arc<dyn Fn(Context, PluginId, Value) -> CommandFuture + Send + Sync>;
type Interfaces = HashMap<(String, String), InterfaceFn>;

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

    /// Calls a method of an interface declared in `[[consumes]]`; the core picks the provider.
    pub async fn call_interface(
        &self,
        interface: InterfaceId,
        method: impl Into<String>,
        params: Value,
    ) -> Result<Value, RpcError> {
        let call = ServiceCallParams {
            interface,
            method: method.into(),
            params,
        };
        self.peer.request::<ServiceCall>(&call).await
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
    interfaces: Interfaces,
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

    /// Handles a method of an interface declared in `[[provides]]` (spec, section 14). The
    /// handler gets the calling plugin and the call's params.
    pub fn interface<F, Fut>(mut self, interface: &str, method: &str, handler: F) -> Self
    where
        F: Fn(Context, PluginId, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, ResponseError>> + Send + 'static,
    {
        let handler: InterfaceFn =
            Arc::new(move |ctx, caller, params| Box::pin(handler(ctx, caller, params)));
        self.interfaces
            .insert((interface.to_owned(), method.to_owned()), handler);
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
        let interfaces = Arc::new(self.interfaces);
        let capabilities = self.capabilities;
        let router_state = state.clone();
        let peer = Peer::spawn(reader, writer, PeerConfig::default(), move |peer| {
            router(peer, router_state, commands, interfaces, capabilities)
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
    interfaces: Arc<Interfaces>,
    capabilities: PluginCapabilities,
) -> Router {
    let init_state = state.clone();
    let invoke_state = state.clone();
    let service_state = state.clone();
    let service_peer = peer.clone();
    Router::new()
        .on_request::<Initialize, _, _>(move |params| {
            let result = if crate::version::compatible(&params.api_version, PROTOCOL_VERSION) {
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
        .on_request::<ServiceInvoke, _, _>(move |call| {
            let init = lock(&service_state.init).clone();
            let key = (call.interface.to_string(), call.method.clone());
            let handler = interfaces.get(&key).cloned();
            let peer = service_peer.clone();
            async move {
                let Some(init) = init else {
                    return Err(ResponseError::new(
                        ErrorCode::InvalidRequest,
                        "not initialized",
                    ));
                };
                let Some(handler) = handler else {
                    return Err(ResponseError::new(
                        ErrorCode::MethodNotFound,
                        format!("{}.{} is not provided by this plugin", key.0, key.1),
                    ));
                };
                handler(Context { peer, init }, call.caller, call.params).await
            }
        })
        .on_request::<Shutdown, _, _>(|()| std::future::ready(Ok(())))
        .on_notification::<Exit, _>(move |()| state.exit.notify_one())
}
