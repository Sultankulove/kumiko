//! Kumiko IDE binary: composition root and CLI.
//!
//! `kumiko [FOLDER]` starts the core on a Tokio runtime, opens `FOLDER` (if given) and runs the
//! workbench on the main thread. `KUMIKO_LOG` sets the log filter (`info` by default).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use kumiko_core::{CoreApi, CoreOptions};
use kumiko_types::KumikoDirs;

#[cfg(feature = "editor-default")]
mod editor;
#[cfg(all(feature = "alt-editor", not(feature = "editor-default")))]
#[path = "editor_alt.rs"]
mod editor;
#[cfg(not(any(feature = "editor-default", feature = "alt-editor")))]
compile_error!(
    "choose an editor: feature `editor-default` or `alt-editor` (docs/sdk/distributions.md)"
);
mod kit;
mod log;
mod terminal;

fn main() -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let mut folder = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let kit_command = args.first().is_some_and(|a| a == "kit");
    for arg in std::env::args_os()
        .skip(1)
        .take(if kit_command { 0 } else { usize::MAX })
    {
        match arg.to_str() {
            Some("--version" | "-V") => {
                println!("kumiko {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some("--help" | "-h") => {
                println!(
                    "usage: kumiko [FOLDER]
       kumiko kit list | apply <path|id> | diff <path|id> | export [id] [name]"
                );
                return Ok(());
            }
            _ => folder = Some(PathBuf::from(arg)),
        }
    }
    // Logging reaches the Output panel from the first line on.
    let output = kumiko_core::output::Output::new();
    log::init(output.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("kumiko-core")
        .build()
        .context("cannot start the async runtime")?;
    let dirs = KumikoDirs::from_env().context("cannot find the user directories")?;
    let mut options = CoreOptions::new(dirs);
    options.output = output;
    options.builtin_resources = builtin_resources();
    options.bundled_plugins = bundled_plugins();
    if options.builtin_resources.is_none() {
        tracing::warn!("built-in resources not found; set KUMIKO_RESOURCES");
    }
    let core = runtime.block_on(CoreApi::start(options))?;
    if kit_command {
        let result = runtime.block_on(kit::run(&core, &args[1..]));
        runtime.block_on(core.shutdown());
        return match result {
            Ok(text) => {
                println!("{text}");
                Ok(())
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2)
            }
        };
    }
    // The window does not wait for the folder: watching a large tree takes seconds on slow
    // file systems (docs/research/ui-linux-native.md). The project tree appears when the
    // folder is added.
    if let Some(folder) = folder {
        let core = core.clone();
        runtime.spawn(async move {
            if let Err(error) = core.open_folder(&folder).await {
                tracing::error!("cannot open {}: {error}", folder.display());
            }
        });
    }

    kumiko_ui::run(
        core.clone(),
        runtime.handle().clone(),
        started,
        std::rc::Rc::new(editor::Editors),
        std::rc::Rc::new(terminal::Terminals),
    );
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

/// `KUMIKO_PLUGINS`, else `plugins/` next to the executable: plugins a distribution ships
/// (one folder per plugin id), where kits install missing plugins from.
fn bundled_plugins() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("KUMIKO_PLUGINS").map(PathBuf::from),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("plugins"))),
    ];
    candidates.into_iter().flatten().find(|dir| dir.is_dir())
}
