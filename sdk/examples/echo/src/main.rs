//! `kumiko.echo`: answers commands so the Plugin Host can be tested against a real process.
//!
//! - `echo.say` returns its arguments;
//! - `echo.whoami` returns the plugin id and process id;
//! - `echo.exit` exits with the given code (a crash, from the host's point of view);
//! - `echo.hang` never answers (timeouts and cancellation);
//! - `echo.call [interface, method, params]` calls an interface through the core;
//! - `echo.spawn` starts a child process that sleeps until killed and returns its pid
//!   (descendants must not outlive the plugin);
//! - interface `kumiko.echo.api`, method `ping`, answers with its params and the caller.
//! - language server `fake`: `kumiko-fake-lsp` installed next to the plugin binary, run in the
//!   workspace folder.

use std::process::{ExitCode, Stdio};

use kumiko_plugin_sdk::process::Plugin;
use kumiko_plugin_sdk::protocol::methods::lsp::LaunchSpec;
use kumiko_plugin_sdk::types::InterfaceId;
use kumiko_plugin_sdk::{ErrorCode, ResponseError};
use serde_json::{Value, json};

/// Set in the environment of the child `echo.spawn` starts.
const SLEEPER: &str = "KUMIKO_ECHO_SLEEPER";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if std::env::var_os(SLEEPER).is_some() {
        std::thread::sleep(std::time::Duration::MAX);
    }
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
        .command("echo.spawn", |_ctx, _args| async move {
            let internal =
                |e: std::io::Error| ResponseError::new(ErrorCode::InternalError, e.to_string());
            let child = std::process::Command::new(std::env::current_exe().map_err(internal)?)
                .env(SLEEPER, "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(internal)?;
            Ok(json!(child.id()))
        })
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
        .language_server("fake", |_ctx, params| async move {
            let internal =
                |e: std::io::Error| ResponseError::new(ErrorCode::InternalError, e.to_string());
            let exe = std::env::current_exe().map_err(internal)?;
            let server =
                exe.with_file_name(format!("kumiko-fake-lsp{}", std::env::consts::EXE_SUFFIX));
            if !server.exists() {
                return Err(ResponseError::new(
                    ErrorCode::NotFound,
                    format!("{} is not installed", server.display()),
                ));
            }
            let command = server.to_string_lossy().into_owned();
            Ok(LaunchSpec {
                command,
                args: Vec::new(),
                env: Default::default(),
                cwd: params
                    .workspace_folder
                    .to_file_path()
                    .map(|p| p.to_string_lossy().into_owned()),
                initialization_options: Some(json!({ "language": params.language_id })),
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
