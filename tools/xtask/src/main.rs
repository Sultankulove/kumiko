//! Build tasks of the repository, in Rust so they run the same on Linux and Windows
//! (ADR-020): `cargo xtask dist` builds the release distribution (spec v1.1, section 7.3).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Plugins shipped with the distribution: package, plugin folder, how it is built.
const PLUGINS: &[(&str, &str, Kind)] = &[
    ("kumiko-java", "plugins/java", Kind::Process),
    ("kumiko-git", "plugins/git", Kind::Wasm),
    ("kumiko-terminal", "plugins/terminal", Kind::Wasm),
];

/// Grammar packages: the parser crate and the library file name the manifest names.
const GRAMMARS: &[(&str, &str, &str)] = &[(
    "kumiko-grammar-java",
    "resources/grammars/java",
    "kumiko_grammar_java",
)];

#[derive(Clone, Copy)]
enum Kind {
    Process,
    Wasm,
}

const WASM_TARGET: &str = "wasm32-wasip2";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("dist") => dist(args.iter().any(|a| a == "--archive")),
        _ => {
            eprintln!(
                "usage: cargo xtask dist [--archive]\n\n  dist       build target/dist/kumiko-<version>-<os>-<arch>/\n  --archive  also pack it (.tar.gz on Linux, .zip on Windows)"
            );
            bail!("unknown task")
        }
    }
}

/// The workspace root: this crate is `tools/xtask`.
fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .unwrap_or(manifest)
        .to_path_buf()
}

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn cargo(args: &[&str]) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    eprintln!("> cargo {}", args.join(" "));
    let status = Command::new(cargo)
        .args(args)
        .current_dir(root())
        .status()
        .context("running cargo")?;
    if !status.success() {
        bail!("cargo {} failed", args.join(" "));
    }
    Ok(())
}

/// Builds everything and lays it out the way `app/kumiko` looks for it: `resources/` and
/// `plugins/<id>/` next to the executable.
fn dist(archive: bool) -> Result<()> {
    let root = root();
    let target = root.join("target");
    let name = format!(
        "kumiko-{}-{}-{}",
        version(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let out = target.join("dist").join(&name);
    if out.exists() {
        std::fs::remove_dir_all(&out).with_context(|| format!("removing {}", out.display()))?;
    }
    std::fs::create_dir_all(&out)?;

    let mut native = vec!["build", "--release", "--locked", "-p", "kumiko"];
    let mut wasm = vec!["build", "--release", "--locked", "--target", WASM_TARGET];
    for (package, _, kind) in PLUGINS {
        match kind {
            Kind::Process => native.extend(["-p", package]),
            Kind::Wasm => wasm.extend(["-p", package]),
        }
    }
    cargo(&native)?;
    cargo(&wasm)?;
    // In a target directory of its own, like the tests do: built with the workspace, the
    // parser's dependencies would get other features.
    let grammar_target = target.join("grammars");
    let grammar_target_arg = grammar_target.to_string_lossy().into_owned();
    for (package, _, _) in GRAMMARS {
        cargo(&[
            "build",
            "--release",
            "--locked",
            "-p",
            package,
            "--target-dir",
            &grammar_target_arg,
        ])?;
    }

    let release = target.join("release");
    let exe = format!("kumiko{}", std::env::consts::EXE_SUFFIX);
    copy_file(&release.join(&exe), &out.join(&exe))?;

    // Resources without the parsers' sources, each grammar with its library.
    copy_tree(&root.join("resources"), &out.join("resources"), &|path| {
        path.file_name().is_some_and(|n| n == "parser")
    })?;
    for (_, dir, library) in GRAMMARS {
        let file = format!(
            "{}{library}{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        );
        copy_file(
            &grammar_target.join("release").join(&file),
            &out.join(dir).join(&file),
        )?;
    }

    for (package, dir, kind) in PLUGINS {
        let source = root.join(dir);
        let id = plugin_id(&source.join("plugin.toml"))?;
        let plugin = out.join("plugins").join(&id);
        copy_file(&source.join("plugin.toml"), &plugin.join("plugin.toml"))?;
        if source.join("schema").is_dir() {
            copy_tree(&source.join("schema"), &plugin.join("schema"), &|_| false)?;
        }
        match kind {
            Kind::Process => {
                let bin = format!("{package}{}", std::env::consts::EXE_SUFFIX);
                copy_file(&release.join(&bin), &plugin.join("bin").join(&bin))?;
            }
            Kind::Wasm => {
                let wasm = format!("{}.wasm", package.replace('-', "_"));
                copy_file(
                    &target.join(WASM_TARGET).join("release").join(wasm),
                    &plugin.join("plugin.wasm"),
                )?;
            }
        }
    }

    for file in ["LICENSE", "README.md", "CHANGELOG.md"] {
        if root.join(file).is_file() {
            copy_file(&root.join(file), &out.join(file))?;
        }
    }
    eprintln!("distribution: {}", out.display());

    if archive {
        let packed = pack(&out, &name)?;
        eprintln!("archive: {}", packed.display());
    }
    Ok(())
}

/// The `id` of the `[plugin]` table; the folder under `plugins/` is named after it.
fn plugin_id(manifest: &Path) -> Result<String> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let mut in_plugin = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_plugin = line == "[plugin]";
        } else if in_plugin && let Some(value) = line.strip_prefix("id") {
            let value = value.trim_start().trim_start_matches('=').trim();
            return Ok(value.trim_matches('"').to_owned());
        }
    }
    bail!("{}: no [plugin] id", manifest.display())
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to)
        .with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
    Ok(())
}

fn copy_tree(from: &Path, to: &Path, skip: &dyn Fn(&Path) -> bool) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let path = entry?.path();
        if skip(&path) {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        if path.is_dir() {
            copy_tree(&path, &to.join(name), skip)?;
        } else {
            copy_file(&path, &to.join(name))?;
        }
    }
    Ok(())
}

/// `.tar.gz` on Unix (keeps the executable bits); on Windows the workflow zips the folder.
fn pack(dir: &Path, name: &str) -> Result<PathBuf> {
    if cfg!(windows) {
        bail!("--archive packs .tar.gz on Unix; on Windows the release workflow makes the .zip");
    }
    let parent = dir.parent().context("distribution folder has no parent")?;
    let path = parent.join(format!("{name}.tar.gz"));
    let file = std::fs::File::create(&path)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    let mut builder = tar::Builder::new(encoder);
    builder.append_dir_all(name, dir)?;
    builder.into_inner()?.finish()?;
    Ok(path)
}
