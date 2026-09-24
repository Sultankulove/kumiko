//! `kumiko.echo`: answers commands so the Plugin Host can be tested against a real process.
//!
//! - `echo.say` returns its arguments;
//! - `echo.whoami` returns the plugin id and process id;
//! - `echo.exit` exits with the given code (a crash, from the host's point of view);
//! - `echo.hang` never answers (timeouts and cancellation);
//! - `echo.call [interface, method, params]` calls an interface through the core;
//! - interface `kumiko.echo.api`, method `ping`, answers with its params and the caller.

use std::process::ExitCode;

use kumiko_plugin_sdk::process::Plugin;
use kumiko_plugin_sdk::types::InterfaceId;
use kumiko_plugin_sdk::{ErrorCode, ResponseError};
use serde_json::{Value, json};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let plugin = Plugin::new()
        .command(
            "echo.say",
            |_ctx, args| async move { Ok(Value::Array(args)) },
        )
        .command("echo.whoami", |ctx, _args| async move {
            Ok(json!({ "id": ctx.plugin_id(), "pid": std::process::id() }))
        })
        .command("echo.exit", |_ctx, args| async move {
            let code = args.first().and_then(Value::as_i64).unwrap_or(1);
            std::process::exit(i32::try_from(code).unwrap_or(1))
        })
        .command("echo.hang", |_ctx, _args| std::future::pending())
        .command("echo.call", |ctx, args| async move {
            let interface = args.first().and_then(Value::as_str).unwrap_or_default();
            let interface = InterfaceId::new(interface)
                .map_err(|e| ResponseError::new(ErrorCode::InvalidParams, e.to_string()))?;
            let method = args
                .get(1)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let params = args.get(2).cloned().unwrap_or(Value::Null);
            ctx.call_interface(interface, method, params)
                .await
                .map_err(|error| match error {
                    kumiko_plugin_sdk::RpcError::Remote(error) => error,
                    other => ResponseError::new(ErrorCode::InternalError, other.to_string()),
                })
        })
        .interface(
            "kumiko.echo.api",
            "ping",
            |_ctx, caller, params| async move { Ok(json!({ "pong": params, "caller": caller })) },
        );
    match plugin.run_stdio().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kumiko-echo: {error}");
            ExitCode::FAILURE
        }
    }
}
