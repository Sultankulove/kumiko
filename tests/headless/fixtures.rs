//! Plugin fixtures built by cargo for the scenarios.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Runs `cargo build` for `package` and returns the artifact of `target` whose file name ends
/// with `extension` (`""` for an executable).
pub fn build(package: &str, extra: &[&str], target: &str, extension: &str) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .args(["build", "-q", "-p", package, "--message-format=json"])
        .args(extra)
        .current_dir(workspace_root())
        .output()
        .expect("cargo runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == target)
        .flat_map(|m| {
            let mut files = vec![m["executable"].clone()];
            files.extend(m["filenames"].as_array().cloned().unwrap_or_default());
            files
        })
        .filter_map(|f| f.as_str().map(PathBuf::from))
        .find(|f| extension.is_empty() || f.to_string_lossy().ends_with(extension))
        .unwrap_or_else(|| panic!("cargo reports no {extension} artifact of {target}"))
}

/// Copies `manifest` (edited) and `files` into `<dir>/<name>`, an installable plugin source.
pub fn plugin_source(
    dir: &Path,
    name: &str,
    manifest: &Path,
    edit: impl FnOnce(String) -> String,
    files: &[(&str, &Path)],
) -> PathBuf {
    let source = dir.join(name);
    std::fs::create_dir_all(&source).unwrap();
    let text = std::fs::read_to_string(manifest).unwrap();
    std::fs::write(source.join("plugin.toml"), edit(text)).unwrap();
    for (target, file) in files {
        let path = source.join(target);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(file, path).unwrap();
    }
    source
}

/// The parser library of `resources/grammars/java`, built next to the test binaries where the
/// Syntax service looks for it in development builds. Once per run, in a target directory of
/// its own: built with the workspace, the parser's dependencies get other features and the
/// library would be relinked by every other build while cores of other tests have it loaded
/// (which Windows refuses). Every test that wants Java highlighting calls this first.
pub fn build_java_grammar() {
    static BUILT: std::sync::Once = std::sync::Once::new();
    BUILT.call_once(|| {
        let deps = std::env::current_exe().unwrap();
        let profile_dir = deps
            .parent()
            .and_then(|d| d.parent())
            .unwrap()
            .to_path_buf();
        let own_target = profile_dir.parent().unwrap().join("fixtures");
        let own_target = own_target.to_string_lossy().into_owned();
        let mut args = vec!["--target-dir", own_target.as_str()];
        if !cfg!(debug_assertions) {
            args.push("--release");
        }
        let built = build(
            "kumiko-grammar-java",
            &args,
            "kumiko_grammar_java",
            std::env::consts::DLL_EXTENSION,
        );
        let target = profile_dir.join(built.file_name().unwrap());
        let same = std::fs::read(&target).ok() == std::fs::read(&built).ok();
        if !same && let Err(error) = std::fs::copy(&built, &target) {
            // Loaded by another test: the library there is the same parser.
            assert!(target.exists(), "cannot place the grammar library: {error}");
        }
    });
}
