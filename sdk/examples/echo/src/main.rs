//! `kumiko.echo`: answers commands so the Plugin Host can be tested against a real process.
//!
//! - `echo.say` returns its arguments;
//! - `echo.whoami` returns the plugin id and process id;
//! - `echo.exit` exits with the given code (a crash, from the host's point of view);
//! - `echo.hang` never answers (timeouts and cancellation).

use std::process::ExitCode;

use kumiko_plugin_sdk::process::Plugin;
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
        .command("echo.hang", |_ctx, _args| std::future::pending());
    match plugin.run_stdio().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kumiko-echo: {error}");
            ExitCode::FAILURE
        }
    }
}
