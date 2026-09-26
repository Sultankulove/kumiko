//! Kumiko IDE binary: composition root and CLI.
//!
//! `kumiko [FOLDER]` starts the core on a Tokio runtime, opens `FOLDER` (if given) and runs the
//! workbench on the main thread. `KUMIKO_LOG` sets the log filter (`info` by default).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use kumiko_core::{CoreApi, CoreOptions};
use kumiko_types::KumikoDirs;

fn main() -> anyhow::Result<()> {
    let mut folder = None;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--version" | "-V") => {
                println!("kumiko {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some("--help" | "-h") => {
                println!("usage: kumiko [FOLDER]");
                return Ok(());
            }
            _ => folder = Some(PathBuf::from(arg)),
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("KUMIKO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("kumiko-core")
        .build()
        .context("cannot start the async runtime")?;
    let dirs = KumikoDirs::from_env().context("cannot find the user directories")?;
    let mut options = CoreOptions::new(dirs);
    options.builtin_resources = builtin_resources();
    if options.builtin_resources.is_none() {
        tracing::warn!("built-in resources not found; set KUMIKO_RESOURCES");
    }
    let core = runtime.block_on(async {
        let core = CoreApi::start(options).await?;
        if let Some(folder) = &folder {
            core.open_folder(folder)
                .await
                .with_context(|| format!("cannot open {}", folder.display()))?;
        }
        anyhow::Ok(core)
    })?;

    kumiko_ui::run(core.clone(), runtime.handle().clone());
    runtime.block_on(core.shutdown());
    Ok(())
}

/// `KUMIKO_RESOURCES`, else `resources/` next to the executable, else the source tree's (a
/// development build run from the repository).
fn builtin_resources() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("KUMIKO_RESOURCES").map(PathBuf::from),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("resources"))),
        Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|dir| dir.join("settings").is_dir())
}
