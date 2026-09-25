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
