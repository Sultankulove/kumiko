//! Storage locations (spec, section 22).
//!
//! Every path the IDE writes to is derived here, so porting to another platform changes
//! only this module. On Linux the XDG base directories are used.

use std::path::{Path, PathBuf};

/// Name of the per-workspace settings directory, `<workspace>/.kumiko/`.
pub const WORKSPACE_DIR: &str = ".kumiko";

const APP_NAME: &str = "kumiko";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathsError {
    #[error("cannot determine the home directory")]
    NoHomeDirectory,
}

/// Root directories of the IDE: configuration, data, state and cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KumikoDirs {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
}

impl KumikoDirs {
    /// Platform directories: `$XDG_CONFIG_HOME/kumiko`, `$XDG_DATA_HOME/kumiko`,
    /// `$XDG_STATE_HOME/kumiko`, `$XDG_CACHE_HOME/kumiko` on Linux. On Windows (ADR-020) only
    /// the configuration roams, `%APPDATA%\kumiko\config`; data, state and cache stay on the
    /// machine, in `%LOCALAPPDATA%\kumiko\{data,state,cache}`.
    pub fn from_env() -> Result<Self, PathsError> {
        let dirs =
            directories::ProjectDirs::from("", "", APP_NAME).ok_or(PathsError::NoHomeDirectory)?;
        // Windows has no state directory; `data_local_dir` is `%LOCALAPPDATA%\kumiko\data`.
        #[cfg(windows)]
        let state = dirs.data_local_dir().with_file_name("state");
        #[cfg(not(windows))]
        let state = dirs
            .state_dir()
            .unwrap_or_else(|| dirs.data_local_dir())
            .to_path_buf();
        Ok(Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_local_dir().to_path_buf(),
            state,
            cache: dirs.cache_dir().to_path_buf(),
        })
    }

    /// All directories under one root: for tests, headless runs and portable installs.
    pub fn with_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
            cache: root.join("cache"),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub fn state_dir(&self) -> &Path {
        &self.state
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    pub fn user_settings_file(&self) -> PathBuf {
        self.config.join("settings.toml")
    }

    pub fn user_keymap_file(&self) -> PathBuf {
        self.config.join("keymap.toml")
    }

    pub fn plugins_dir(&self) -> PathBuf {
        self.data.join("plugins")
    }

    /// Enabled/disabled state of installed plugins, kept outside plugin directories.
    pub fn plugins_state_file(&self) -> PathBuf {
        self.plugins_dir().join("state.toml")
    }

    pub fn resources_dir(&self) -> PathBuf {
        self.data.join("resources")
    }

    pub fn kits_dir(&self) -> PathBuf {
        self.data.join("kits")
    }

    /// Layout state of workspaces, one `<hash>.json` per workspace.
    pub fn workspaces_state_dir(&self) -> PathBuf {
        self.state.join("workspaces")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.state.join("logs")
    }

    /// Backups of unsaved edits.
    pub fn backup_dir(&self) -> PathBuf {
        self.state.join("backup")
    }

    /// Compiled WASM plugin modules.
    pub fn wasm_cache_dir(&self) -> PathBuf {
        self.cache.join("wasm")
    }

    /// Cache directory owned by one plugin, e.g. for downloaded language servers.
    pub fn plugin_cache_dir(&self, plugin: &crate::PluginId) -> PathBuf {
        self.cache.join("plugins").join(plugin.as_str())
    }
}

/// `<workspace>/.kumiko/`
pub fn workspace_settings_dir(workspace_root: impl AsRef<Path>) -> PathBuf {
    workspace_root.as_ref().join(WORKSPACE_DIR)
}

/// `std::fs::canonicalize`, but on Windows without the `\\?\` prefix it puts before drive
/// paths: compilers and language servers started with such a path often reject it.
pub fn canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let path = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    if let Some(local) = path.to_str().and_then(|p| p.strip_prefix(r"\\?\"))
        && let [drive, b':', b'\\', ..] = local.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        return Ok(PathBuf::from(local));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginId;

    #[test]
    fn layout_under_root() {
        let dirs = KumikoDirs::with_root("/r");
        assert_eq!(
            dirs.user_settings_file(),
            Path::new("/r/config/settings.toml")
        );
        assert_eq!(dirs.user_keymap_file(), Path::new("/r/config/keymap.toml"));
        assert_eq!(
            dirs.plugins_state_file(),
            Path::new("/r/data/plugins/state.toml")
        );
        assert_eq!(dirs.resources_dir(), Path::new("/r/data/resources"));
        assert_eq!(dirs.kits_dir(), Path::new("/r/data/kits"));
        assert_eq!(
            dirs.workspaces_state_dir(),
            Path::new("/r/state/workspaces")
        );
        assert_eq!(dirs.logs_dir(), Path::new("/r/state/logs"));
        assert_eq!(dirs.backup_dir(), Path::new("/r/state/backup"));
        assert_eq!(dirs.wasm_cache_dir(), Path::new("/r/cache/wasm"));
        let plugin = PluginId::new("org.example.tool").unwrap();
        assert_eq!(
            dirs.plugin_cache_dir(&plugin),
            Path::new("/r/cache/plugins/org.example.tool")
        );
        assert_eq!(workspace_settings_dir("/w"), Path::new("/w/.kumiko"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_uses_xdg_directories() {
        // Only the trailing component is asserted: the XDG variables differ between machines.
        let dirs = KumikoDirs::from_env().unwrap();
        for dir in [
            dirs.config_dir(),
            dirs.data_dir(),
            dirs.state_dir(),
            dirs.cache_dir(),
        ] {
            assert_eq!(dir.file_name().unwrap(), APP_NAME, "{}", dir.display());
        }
        assert_ne!(dirs.config_dir(), dirs.data_dir());
    }

    #[cfg(windows)]
    #[test]
    fn windows_keeps_only_configuration_roaming() {
        let dirs = KumikoDirs::from_env().unwrap();
        let roaming = std::env::var_os("APPDATA").unwrap();
        let local = std::env::var_os("LOCALAPPDATA").unwrap();
        assert_eq!(
            dirs.config_dir(),
            Path::new(&roaming).join(r"kumiko\config")
        );
        for (dir, name) in [
            (dirs.data_dir(), "data"),
            (dirs.state_dir(), "state"),
            (dirs.cache_dir(), "cache"),
        ] {
            assert_eq!(dir, Path::new(&local).join("kumiko").join(name));
        }
    }
}
