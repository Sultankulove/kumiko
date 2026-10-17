//! Eclipse JDT Language Server: where an install is and how to start it. An install is a
//! folder with `plugins/org.eclipse.equinox.launcher_*.jar` and `config_<os>/`.

use std::path::{Path, PathBuf};

/// The smallest JDK that runs JDT LS.
pub const MIN_JAVA: u32 = 21;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub root: PathBuf,
    pub launcher: PathBuf,
    /// `config_win`, `config_linux`, `config_mac`, or the `_arm` variants.
    pub config: PathBuf,
}

fn config_name() -> &'static str {
    let arm = cfg!(target_arch = "aarch64");
    match (std::env::consts::OS, arm) {
        ("windows", _) => "config_win",
        ("macos", true) => "config_mac_arm",
        ("macos", false) => "config_mac",
        (_, true) => "config_linux_arm",
        _ => "config_linux",
    }
}

/// The install at `root`, if it is one.
pub fn find(root: &Path) -> Option<Install> {
    let plugins = std::fs::read_dir(root.join("plugins")).ok()?;
    let mut launchers: Vec<PathBuf> = plugins
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("org.eclipse.equinox.launcher_") && n.ends_with(".jar")
            })
        })
        .collect();
    launchers.sort();
    let launcher = launchers.pop()?;
    let config = root.join(config_name());
    config.is_dir().then(|| Install {
        root: root.to_path_buf(),
        launcher,
        config,
    })
}

/// Installs downloaded into `cache` (`jdtls/<version>/`), newest version first.
pub fn cached(cache: &Path) -> Vec<Install> {
    let Ok(entries) = std::fs::read_dir(cache.join("jdtls")) else {
        return Vec::new();
    };
    let mut found: Vec<Install> = entries.flatten().filter_map(|e| find(&e.path())).collect();
    found.sort_by(|a, b| b.root.cmp(&a.root));
    found
}

/// Command-line arguments after `java`: the JVM options JDT LS needs, the launcher, its
/// configuration, and the workspace data folder (one per project).
pub fn arguments(install: &Install, data: &Path) -> Vec<String> {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    vec![
        "-Declipse.application=org.eclipse.jdt.ls.core.id1".into(),
        "-Dosgi.bundles.defaultStartLevel=4".into(),
        "-Declipse.product=org.eclipse.jdt.ls.core.product".into(),
        "-Dlog.level=ALL".into(),
        // .project, .classpath and .settings go to the data folder, not into the project.
        "-Djava.import.generatesMetadataFilesAtProjectRoot=false".into(),
        "-Xmx1G".into(),
        "--add-modules=ALL-SYSTEM".into(),
        "--add-opens".into(),
        "java.base/java.util=ALL-UNNAMED".into(),
        "--add-opens".into(),
        "java.base/java.lang=ALL-UNNAMED".into(),
        "-jar".into(),
        path(&install.launcher),
        "-configuration".into(),
        path(&install.config),
        "-data".into(),
        path(data),
    ]
}

/// A stable folder name for a project: JDT LS keeps its index per workspace folder.
pub fn workspace_key(folder: &str) -> String {
    // FNV-1a: stable across runs and Rust versions, unlike `DefaultHasher`.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in folder.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn fake_install(root: &Path) {
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::create_dir_all(root.join(config_name())).unwrap();
        for name in [
            "org.eclipse.equinox.launcher_1.6.0.v1.jar",
            "org.eclipse.equinox.launcher_1.8.0.v2.jar",
            "org.eclipse.jdt.ls.core_1.0.jar",
        ] {
            std::fs::write(root.join("plugins").join(name), "").unwrap();
        }
    }

    #[test]
    fn finds_the_newest_launcher_and_the_os_configuration() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(find(temp.path()), None);
        let root = temp.path().join("jdtls").join("1.40.0");
        fake_install(&root);
        let install = find(&root).unwrap();
        assert!(
            install
                .launcher
                .ends_with("org.eclipse.equinox.launcher_1.8.0.v2.jar")
        );
        assert!(install.config.ends_with(config_name()));
        assert_eq!(cached(temp.path()), vec![install.clone()]);
        let args = arguments(&install, Path::new("data"));
        assert_eq!(args.iter().filter(|a| *a == "-jar").count(), 1);
        assert_eq!(args.last().map(String::as_str), Some("data"));
    }

    #[test]
    fn workspace_keys_are_stable() {
        assert_eq!(workspace_key("file:///w"), workspace_key("file:///w"));
        assert_ne!(workspace_key("file:///w"), workspace_key("file:///v"));
        assert_eq!(workspace_key("").len(), 16);
    }
}
