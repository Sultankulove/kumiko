//! A process plugin served over an in-memory pipe, driven by a test host.

// Test harness: helpers outside `#[test]` functions may panic too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::{Value, json};
use tokio::task::JoinHandle;

use kumiko_ipc::{Peer, PeerConfig, Router};
use kumiko_plugin_sdk::process::{Plugin, SdkError};
use kumiko_plugin_sdk::{ErrorCode, ResponseError};
use kumiko_protocol::PROTOCOL_VERSION;
use kumiko_protocol::common::CommandCall;
use kumiko_protocol::methods::command::{ExecuteCommand, InvokeCommand};
use kumiko_protocol::methods::lifecycle::{
    Exit, HostCapabilities, Initialize, InitializeParams, Initialized, Shutdown, WorkspaceInfo,
};
use kumiko_types::{CommandId, PluginId};

fn plugin() -> Plugin {
    Plugin::new()
        .command(
            "test.echo",
            |_ctx, args| async move { Ok(Value::Array(args)) },
        )
        .command("test.whoami", |ctx, _args| async move {
            Ok(json!({ "id": ctx.plugin_id(), "locale": ctx.params().locale }))
        })
        .command("test.askHost", |ctx, args| async move {
            ctx.execute_command(CommandId::new("host.double").unwrap(), args)
                .await
                .map_err(|error| ResponseError::new(ErrorCode::InternalError, error.to_string()))
        })
}

/// Starts the plugin and a host peer that implements `command/execute` for `host.double`.
fn connect(plugin: Plugin) -> (Peer, JoinHandle<Result<(), SdkError>>) {
    let (host_side, plugin_side) = tokio::io::duplex(64 * 1024);
    let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
    let served = tokio::spawn(plugin.serve(plugin_read, plugin_write));
    let (host_read, host_write) = tokio::io::split(host_side);
    let host = Peer::spawn(host_read, host_write, PeerConfig::default(), |_| {
        Router::new().on_request::<ExecuteCommand, _, _>(|call: CommandCall| async move {
            let n = call.args.first().and_then(Value::as_i64).unwrap_or(0);
            Ok(json!(n * 2))
        })
    });
    (host, served)
}

fn init_params(api_version: &str) -> InitializeParams {
    InitializeParams {
        api_version: api_version.to_owned(),
        plugin_id: PluginId::new("test.plugin").unwrap(),
        host_capabilities: HostCapabilities::default(),
        workspace: WorkspaceInfo::default(),
        config: json!({}),
        locale: "ru".to_owned(),
    }
}

async fn invoke(
    host: &Peer,
    command: &str,
    args: Vec<Value>,
) -> Result<Value, kumiko_ipc::RpcError> {
    host.request::<InvokeCommand>(&CommandCall {
        command: CommandId::new(command).unwrap(),
        args,
    })
    .await
}

#[tokio::test]
async fn lifecycle_commands_and_host_calls() {
    let (host, served) = connect(plugin());

    // Commands before the handshake are refused.
    let early = invoke(&host, "test.echo", vec![]).await.unwrap_err();
    assert_eq!(early.code(), Some(ErrorCode::InvalidRequest));

    let result = host
        .request::<Initialize>(&init_params(PROTOCOL_VERSION))
        .await
        .unwrap();
    assert_eq!(result.api_version, PROTOCOL_VERSION);
    host.notify::<Initialized>(&()).await.unwrap();

    assert_eq!(
        invoke(&host, "test.echo", vec![json!(1), json!("a")])
            .await
            .unwrap(),
        json!([1, "a"])
    );
    assert_eq!(
        invoke(&host, "test.whoami", vec![]).await.unwrap(),
        json!({ "id": "test.plugin", "locale": "ru" })
    );
    // The plugin calls back into the host while handling a request.
    assert_eq!(
        invoke(&host, "test.askHost", vec![json!(21)])
            .await
            .unwrap(),
        json!(42)
    );

    let unknown = invoke(&host, "test.unknown", vec![]).await.unwrap_err();
    assert_eq!(unknown.code(), Some(ErrorCode::NotFound));

    host.request::<Shutdown>(&()).await.unwrap();
    host.notify::<Exit>(&()).await.unwrap();
    served.await.unwrap().unwrap();
}

#[tokio::test]
async fn incompatible_host_fails_the_handshake() {
    let (host, _served) = connect(plugin());
    let error = host
        .request::<Initialize>(&init_params("7.0.0"))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::ApiVersionMismatch));
}

#[tokio::test]
async fn host_going_away_ends_the_plugin() {
    let (host, served) = connect(plugin());
    host.request::<Initialize>(&init_params(PROTOCOL_VERSION))
        .await
        .unwrap();
    host.close();
    drop(host);
    assert!(matches!(
        served.await.unwrap(),
        Err(SdkError::ConnectionClosed)
    ));
}
