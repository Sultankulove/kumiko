//! Kits (spec, section 15): built-in ones are resource packages (`resources/kits/<id>`), users
//! add `*.kit.toml` files to `<data>/kits` or name a file. Applying installs what is missing,
//! enables and disables plugins, and sets the kit's settings layer. The applied kit is kept
//! in `<data>/kits/active.kit.toml` and read again at the next start.

use std::path::{Path, PathBuf};

use kumiko_plugin_host::kit::{Kit, KitError, Plan};
use kumiko_resources::ResourceKind;
use serde_json::{Map, Value};

use crate::{CoreApi, CoreError};

/// A kit offered to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KitInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Its file; the built-in ones live in the resources.
    pub path: PathBuf,
    pub builtin: bool,
}

fn kit_error(error: KitError) -> CoreError {
    let KitError::Invalid { path, message } = error;
    CoreError::Io {
        path: PathBuf::from(path),
        message,
    }
}

const ACTIVE_FILE: &str = "active.kit.toml";

impl CoreApi {
    /// Built-in kits, then the user's (`<data>/kits/*.kit.toml`); a later id replaces an
    /// earlier one.
    pub fn kits(&self) -> Vec<KitInfo> {
        let mut found: Vec<KitInfo> = Vec::new();
        let mut add = |info: KitInfo| {
            found.retain(|k| k.id != info.id);
            found.push(info);
        };
        for package in self.inner.resources.list(ResourceKind::Kit) {
            if let Ok(kit) = Kit::load(&package.manifest_path()) {
                add(KitInfo {
                    id: kit.id,
                    name: kit.name,
                    description: kit.description,
                    path: package.manifest_path(),
                    builtin: true,
                });
            }
        }
        if let Ok(entries) = std::fs::read_dir(self.inner.options.dirs.kits_dir()) {
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".kit.toml") && n != ACTIVE_FILE)
                })
                .collect();
            files.sort();
            for path in files {
                if let Ok(kit) = Kit::load(&path) {
                    add(KitInfo {
                        id: kit.id,
                        name: kit.name,
                        description: kit.description,
                        path,
                        builtin: false,
                    });
                }
            }
        }
        found
    }

    /// A kit by file path or by id (`minimal`, `java`, `kumiko.kit.java`).
    pub fn load_kit(&self, spec: &str) -> Result<Kit, CoreError> {
        let path = Path::new(spec);
        if path.is_file() {
            return Kit::load(path).map_err(kit_error);
        }
        let known = self.kits();
        let wanted = known
            .iter()
            .find(|k| k.id == spec || k.id.rsplit('.').next() == Some(spec))
            .ok_or_else(|| CoreError::Io {
                path: PathBuf::from(spec),
                message: format!(
                    "no kit with this path or id; known: {}",
                    known
                        .iter()
                        .map(|k| k.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })?;
        Kit::load(&wanted.path).map_err(kit_error)
    }

    /// What applying the kit would do.
    pub fn plan_kit(&self, kit: &Kit) -> Plan {
        kit.plan(
            &self.plugins(),
            self.inner.options.bundled_plugins.as_deref(),
        )
    }

    /// Applies the kit: installs, enables and disables plugins, sets its settings layer and
    /// remembers it. Applying it again changes nothing.
    pub async fn apply_kit(&self, kit: &Kit) -> Result<Plan, CoreError> {
        let plan = self.plan_kit(kit);
        for (id, source) in &plan.install {
            tracing::info!(plugin = %id, source = %source.display(), "kit: installing");
            self.install_plugin(source).await?;
        }
        for id in &plan.enable {
            self.set_plugin_enabled(id, true).await?;
        }
        for id in &plan.disable {
            self.set_plugin_enabled(id, false).await?;
        }
        self.inner.config.set_kit(kit.settings.clone());
        self.remember_kit(kit)?;
        Ok(plan)
    }

    fn remember_kit(&self, kit: &Kit) -> Result<(), CoreError> {
        let dir = self.inner.options.dirs.kits_dir();
        let path = dir.join(ACTIVE_FILE);
        let text = kit.to_toml();
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, text))
            .map_err(|e| CoreError::Io {
                path,
                message: e.to_string(),
            })
    }

    /// At start: the settings of the kit applied last.
    pub(crate) fn restore_kit(&self) {
        let path = self.inner.options.dirs.kits_dir().join(ACTIVE_FILE);
        if !path.is_file() {
            return;
        }
        match Kit::load(&path) {
            Ok(kit) => self.inner.config.set_kit(kit.settings),
            Err(error) => tracing::warn!("the applied kit cannot be read: {error}"),
        }
    }

    /// The id of the kit applied last.
    pub fn active_kit(&self) -> Option<String> {
        Kit::load(&self.inner.options.dirs.kits_dir().join(ACTIVE_FILE))
            .ok()
            .map(|k| k.id)
    }

    /// A kit file describing the current state: the enabled plugins and the applied kit's
    /// settings.
    pub fn export_kit(&self, id: &str, name: &str) -> String {
        let settings: Map<String, Value> =
            Kit::load(&self.inner.options.dirs.kits_dir().join(ACTIVE_FILE))
                .map(|k| k.settings)
                .unwrap_or_default();
        Kit::export(id, name, &self.plugins(), &settings)
    }
}
