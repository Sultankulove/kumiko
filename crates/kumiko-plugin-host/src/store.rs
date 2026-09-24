//! Installed plugins on disk: `plugins/<id>/<version>/` and `plugins/state.toml` (spec,
//! section 13).
//!
//! The store only moves directories and remembers enabled flags. Whether an installed plugin can
//! run (KPP version, dependencies, required interfaces) is decided when plugins are loaded: an
//! incompatible plugin stays installed with a reason.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use kumiko_types::{KumikoDirs, PluginId};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::manifest::{MANIFEST_FILE, Manifest, ManifestError};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path}: {source}")]
    Manifest {
        path: PathBuf,
        source: ManifestError,
    },
    #[error("{path}: {reason}")]
    State { path: PathBuf, reason: String },
    #[error("{id} {version} is already installed")]
    AlreadyInstalled { id: PluginId, version: Version },
    #[error("{0} is not installed")]
    NotInstalled(PluginId),
    #[error("entry `{entry}` is missing in {dir}")]
    MissingEntry { dir: PathBuf, entry: String },
    #[error("{0}: symbolic links are not allowed in plugins")]
    Symlink(PathBuf),
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_owned(),
        source,
    }
}

/// A plugin found in the store.
#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    pub manifest: Manifest,
    /// `plugins/<id>/<version>/`.
    pub dir: PathBuf,
    pub enabled: bool,
}

/// Result of reading the store: broken plugin directories do not hide the rest.
#[derive(Debug, Default)]
pub struct Scan {
    pub plugins: Vec<InstalledPlugin>,
    pub problems: Vec<StoreError>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    #[serde(default)]
    plugins: BTreeMap<PluginId, PluginState>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginState {
    enabled: bool,
}

/// Plugin directories and their enabled flags.
#[derive(Debug, Clone)]
pub struct PluginStore {
    root: PathBuf,
    state_file: PathBuf,
}

impl PluginStore {
    pub fn new(dirs: &KumikoDirs) -> Self {
        Self::with_paths(dirs.plugins_dir(), dirs.plugins_state_file())
    }

    pub fn with_paths(root: impl Into<PathBuf>, state_file: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state_file: state_file.into(),
        }
    }

    /// Reads every `<id>/<version>/plugin.toml`. When several versions of one plugin are present
    /// (an interrupted upgrade), the highest wins.
    pub fn scan(&self) -> Scan {
        let mut scan = Scan::default();
        let state = match self.read_state() {
            Ok(state) => state,
            Err(error) => {
                scan.problems.push(error);
                StateFile::default()
            }
        };
        let mut latest: BTreeMap<PluginId, InstalledPlugin> = BTreeMap::new();
        for dir in self.version_dirs(&mut scan.problems) {
            match read_manifest(&dir) {
                Ok(manifest) => {
                    let enabled = state.plugins.get(manifest.id()).is_none_or(|s| s.enabled);
                    let plugin = InstalledPlugin {
                        manifest,
                        dir,
                        enabled,
                    };
                    let id = plugin.manifest.id().clone();
                    match latest.get(&id) {
                        Some(known)
                            if known.manifest.plugin.version >= plugin.manifest.plugin.version => {}
                        _ => {
                            latest.insert(id, plugin);
                        }
                    }
                }
                Err(error) => scan.problems.push(error),
            }
        }
        scan.plugins = latest.into_values().collect();
        scan
    }

    /// Copies the plugin in `source` into the store. Installing another version of an installed
    /// plugin replaces it; the enabled flag is kept.
    pub fn install(&self, source: &Path) -> Result<InstalledPlugin, StoreError> {
        let manifest = read_manifest(source)?;
        if let Some(entry) = &manifest.plugin.entry
            && !entry.resolve(source).is_file()
        {
            return Err(StoreError::MissingEntry {
                dir: source.to_owned(),
                entry: entry.as_str().to_owned(),
            });
        }
        let id = manifest.id().clone();
        let version = manifest.plugin.version.clone();
        let plugin_dir = self.root.join(id.as_str());
        let target = plugin_dir.join(version.to_string());
        if target.exists() {
            return Err(StoreError::AlreadyInstalled { id, version });
        }

        // Copy next to the target and rename, so a half-copied plugin is never scanned.
        fs::create_dir_all(&plugin_dir).map_err(io_error(&plugin_dir))?;
        let staging = plugin_dir.join(format!(".installing-{version}-{}", std::process::id()));
        if staging.exists() {
            fs::remove_dir_all(&staging).map_err(io_error(&staging))?;
        }
        if let Err(error) = copy_tree(source, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        fs::rename(&staging, &target).map_err(io_error(&target))?;

        for old in subdirs(&plugin_dir)? {
            if old != target {
                fs::remove_dir_all(&old).map_err(io_error(&old))?;
            }
        }
        let enabled = self.is_enabled(&id)?;
        Ok(InstalledPlugin {
            manifest,
            dir: target,
            enabled,
        })
    }

    /// Removes every version of the plugin and forgets its enabled flag.
    pub fn uninstall(&self, id: &PluginId) -> Result<(), StoreError> {
        let plugin_dir = self.root.join(id.as_str());
        if !plugin_dir.is_dir() {
            return Err(StoreError::NotInstalled(id.clone()));
        }
        fs::remove_dir_all(&plugin_dir).map_err(io_error(&plugin_dir))?;
        let mut state = self.read_state()?;
        if state.plugins.remove(id).is_some() {
            self.write_state(&state)?;
        }
        Ok(())
    }

    pub fn set_enabled(&self, id: &PluginId, enabled: bool) -> Result<(), StoreError> {
        if !self.root.join(id.as_str()).is_dir() {
            return Err(StoreError::NotInstalled(id.clone()));
        }
        let mut state = self.read_state()?;
        state.plugins.insert(id.clone(), PluginState { enabled });
        self.write_state(&state)
    }

    /// Plugins are enabled unless the state file says otherwise.
    pub fn is_enabled(&self, id: &PluginId) -> Result<bool, StoreError> {
        Ok(self.read_state()?.plugins.get(id).is_none_or(|s| s.enabled))
    }

    fn read_state(&self) -> Result<StateFile, StoreError> {
        let text = match fs::read_to_string(&self.state_file) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(StateFile::default());
            }
            Err(error) => return Err(io_error(&self.state_file)(error)),
        };
        toml::from_str(&text).map_err(|error| StoreError::State {
            path: self.state_file.clone(),
            reason: error.to_string(),
        })
    }

    fn write_state(&self, state: &StateFile) -> Result<(), StoreError> {
        let path = &self.state_file;
        let text = toml::to_string(state).map_err(|error| StoreError::State {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error(parent))?;
        }
        let temp = path.with_extension("toml.tmp");
        fs::write(&temp, text).map_err(io_error(&temp))?;
        fs::rename(&temp, path).map_err(io_error(path))
    }

    /// `<root>/<id>/<version>` directories; staging leftovers (dot names) are skipped.
    fn version_dirs(&self, problems: &mut Vec<StoreError>) -> Vec<PathBuf> {
        if !self.root.is_dir() {
            return Vec::new();
        }
        let mut dirs = Vec::new();
        let plugins = match subdirs(&self.root) {
            Ok(plugins) => plugins,
            Err(error) => {
                problems.push(error);
                return dirs;
            }
        };
        for plugin in plugins {
            match subdirs(&plugin) {
                Ok(versions) => dirs.extend(versions),
                Err(error) => problems.push(error),
            }
        }
        dirs
    }
}

fn read_manifest(dir: &Path) -> Result<Manifest, StoreError> {
    let path = dir.join(MANIFEST_FILE);
    let text = fs::read_to_string(&path).map_err(io_error(&path))?;
    Manifest::parse(&text).map_err(|source| StoreError::Manifest { path, source })
}

/// Sorted subdirectories whose names do not start with a dot.
fn subdirs(dir: &Path) -> Result<Vec<PathBuf>, StoreError> {
    let mut result = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_error(dir))? {
        let entry = entry.map_err(io_error(dir))?;
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        let path = entry.path();
        if !hidden && path.is_dir() {
            result.push(path);
        }
    }
    result.sort();
    Ok(result)
}

/// Copies a directory tree, keeping file permissions (the entry must stay executable).
fn copy_tree(from: &Path, to: &Path) -> Result<(), StoreError> {
    fs::create_dir_all(to).map_err(io_error(to))?;
    for entry in fs::read_dir(from).map_err(io_error(from))? {
        let entry = entry.map_err(io_error(from))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type().map_err(io_error(&source))?;
        if kind.is_symlink() {
            return Err(StoreError::Symlink(source));
        } else if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            fs::copy(&source, &target).map_err(io_error(&source))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _temp: tempfile::TempDir,
        home: PathBuf,
        store: PluginStore,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().to_owned();
        let store = PluginStore::with_paths(home.join("plugins"), home.join("plugins/state.toml"));
        Fixture {
            _temp: temp,
            home,
            store,
        }
    }

    /// Writes a process plugin with an entry script into `<home>/src/<name>`.
    fn source(home: &Path, name: &str, id: &str, version: &str) -> PathBuf {
        let dir = home.join("src").join(name);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(
            dir.join(MANIFEST_FILE),
            format!(
                "[plugin]\nid = \"{id}\"\nname = \"Echo\"\nversion = \"{version}\"\napi = \"^0.1\"\nkind = \"process\"\nentry = \"bin/echo\"\n"
            ),
        )
        .unwrap();
        fs::write(dir.join("bin/echo"), "#!/bin/sh\n").unwrap();
        dir
    }

    fn id(value: &str) -> PluginId {
        PluginId::new(value).unwrap()
    }

    #[test]
    fn install_scan_uninstall() {
        let f = fixture();
        assert!(f.store.scan().plugins.is_empty());

        let installed = f
            .store
            .install(&source(&f.home, "echo", "test.echo", "0.1.0"))
            .unwrap();
        assert_eq!(installed.dir, f.home.join("plugins/test.echo/0.1.0"));
        assert!(installed.enabled);
        assert!(installed.dir.join("bin/echo").is_file());

        let scan = f.store.scan();
        assert!(scan.problems.is_empty(), "{:?}", scan.problems);
        assert_eq!(scan.plugins.len(), 1);
        assert_eq!(scan.plugins[0].manifest.id().as_str(), "test.echo");

        f.store.uninstall(&id("test.echo")).unwrap();
        assert!(f.store.scan().plugins.is_empty());
        assert!(matches!(
            f.store.uninstall(&id("test.echo")),
            Err(StoreError::NotInstalled(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn install_keeps_the_entry_executable() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        let src = source(&f.home, "echo", "test.echo", "0.1.0");
        fs::set_permissions(src.join("bin/echo"), fs::Permissions::from_mode(0o755)).unwrap();
        let installed = f.store.install(&src).unwrap();
        let mode = fs::metadata(installed.dir.join("bin/echo"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }

    #[test]
    fn upgrade_replaces_the_old_version_and_keeps_the_flag() {
        let f = fixture();
        f.store
            .install(&source(&f.home, "v1", "test.echo", "0.1.0"))
            .unwrap();
        f.store.set_enabled(&id("test.echo"), false).unwrap();

        let same = f
            .store
            .install(&source(&f.home, "v1", "test.echo", "0.1.0"));
        assert!(matches!(same, Err(StoreError::AlreadyInstalled { .. })));

        let upgraded = f
            .store
            .install(&source(&f.home, "v2", "test.echo", "0.2.0"))
            .unwrap();
        assert!(!upgraded.enabled);
        assert!(!f.home.join("plugins/test.echo/0.1.0").exists());
        let scan = f.store.scan();
        assert_eq!(scan.plugins.len(), 1);
        assert_eq!(
            scan.plugins[0].manifest.plugin.version,
            Version::new(0, 2, 0)
        );
    }

    #[test]
    fn enabled_flags_live_in_the_state_file() {
        let f = fixture();
        f.store
            .install(&source(&f.home, "a", "test.a", "1.0.0"))
            .unwrap();
        f.store
            .install(&source(&f.home, "b", "test.b", "1.0.0"))
            .unwrap();
        f.store.set_enabled(&id("test.b"), false).unwrap();

        let reopened =
            PluginStore::with_paths(f.home.join("plugins"), f.home.join("plugins/state.toml"));
        let flags: Vec<_> = reopened
            .scan()
            .plugins
            .iter()
            .map(|p| (p.manifest.id().to_string(), p.enabled))
            .collect();
        assert_eq!(
            flags,
            [("test.a".to_owned(), true), ("test.b".to_owned(), false)]
        );
        let state = fs::read_to_string(f.home.join("plugins/state.toml")).unwrap();
        assert!(
            state.contains("test.b") && state.contains("enabled = false"),
            "{state}"
        );

        f.store.uninstall(&id("test.b")).unwrap();
        let state = fs::read_to_string(f.home.join("plugins/state.toml")).unwrap();
        assert!(!state.contains("test.b"), "{state}");
        assert!(matches!(
            f.store.set_enabled(&id("test.b"), true),
            Err(StoreError::NotInstalled(_))
        ));
    }

    #[test]
    fn rejects_broken_sources() {
        let f = fixture();
        let no_entry = source(&f.home, "no-entry", "test.echo", "0.1.0");
        fs::remove_file(no_entry.join("bin/echo")).unwrap();
        assert!(matches!(
            f.store.install(&no_entry),
            Err(StoreError::MissingEntry { .. })
        ));

        let bad = source(&f.home, "bad", "test.bad", "0.1.0");
        fs::write(bad.join(MANIFEST_FILE), "[plugin]\nid = \"bad\"\n").unwrap();
        assert!(matches!(
            f.store.install(&bad),
            Err(StoreError::Manifest { .. })
        ));

        assert!(matches!(
            f.store.install(&f.home.join("src/missing")),
            Err(StoreError::Io { .. })
        ));
        assert!(!f.home.join("plugins/test.echo").join("0.1.0").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_and_leaves_nothing_behind() {
        let f = fixture();
        let src = source(&f.home, "link", "test.link", "0.1.0");
        std::os::unix::fs::symlink("/etc/passwd", src.join("passwd")).unwrap();
        assert!(matches!(f.store.install(&src), Err(StoreError::Symlink(_))));
        let leftovers: Vec<_> = fs::read_dir(f.home.join("plugins/test.link"))
            .unwrap()
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn broken_plugins_do_not_hide_the_rest() {
        let f = fixture();
        f.store
            .install(&source(&f.home, "ok", "test.ok", "1.0.0"))
            .unwrap();
        let broken = f.home.join("plugins/test.broken/1.0.0");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join(MANIFEST_FILE), "not toml [").unwrap();
        fs::write(f.home.join("plugins/state.toml"), "garbage = [").unwrap();

        let scan = f.store.scan();
        assert_eq!(scan.plugins.len(), 1);
        assert!(scan.plugins[0].enabled);
        assert_eq!(scan.problems.len(), 2, "{:?}", scan.problems);
    }
}
