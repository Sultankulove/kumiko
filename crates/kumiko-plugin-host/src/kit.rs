//! Kits (spec, section 15): a declarative file naming plugins, L3 resources and default
//! settings. Applying one is idempotent; before it runs, the plan says what will be installed,
//! enabled and disabled. This module is pure data: parsing, planning, export. The core does
//! the installing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::{Map, Value};

use kumiko_types::PluginId;

use crate::registry::{PluginRegistry, PluginStatus};

#[derive(Debug, thiserror::Error)]
pub enum KitError {
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    kit: Header,
    #[serde(default)]
    plugins: BTreeMap<String, String>,
    #[serde(default)]
    sources: BTreeMap<String, Source>,
    #[serde(default)]
    resources: Resources,
    #[serde(default)]
    settings: toml::Table,
    /// Present when the kit is a resource (`resource.toml` of a built-in kit).
    #[serde(default)]
    resource: Option<toml::Table>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    kumiko: Option<String>,
    #[serde(default)]
    description: String,
    /// Disable the plugins the kit does not list.
    #[serde(default)]
    exclusive: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default)]
    pub grammars: Vec<String>,
    pub theme: Option<String>,
    pub icons: Option<String>,
    pub keymap: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Kit {
    pub id: String,
    pub name: String,
    pub version: Version,
    pub description: String,
    pub exclusive: bool,
    /// The IDE versions it was made for.
    pub kumiko: Option<VersionReq>,
    pub plugins: BTreeMap<PluginId, VersionReq>,
    /// Where to find a plugin that is not installed (directories, relative to the kit).
    pub sources: BTreeMap<PluginId, PathBuf>,
    pub resources: Resources,
    /// Default settings (flat or nested keys), a layer below the user's.
    pub settings: Map<String, Value>,
}

fn invalid(path: &str, message: impl std::fmt::Display) -> KitError {
    KitError::Invalid {
        path: path.to_owned(),
        message: message.to_string(),
    }
}

impl Kit {
    /// Parses a kit; `base` is the folder `[sources]` paths are relative to.
    pub fn parse(text: &str, base: &Path, name: &str) -> Result<Self, KitError> {
        let raw: Raw = toml::from_str(text).map_err(|e| invalid(name, e))?;
        let version = Version::parse(&raw.kit.version)
            .map_err(|e| invalid(name, format!("kit.version: {e}")))?;
        let kumiko = raw
            .kit
            .kumiko
            .as_deref()
            .map(VersionReq::parse)
            .transpose()
            .map_err(|e| invalid(name, format!("kit.kumiko: {e}")))?;
        let mut plugins = BTreeMap::new();
        for (id, req) in raw.plugins {
            let id = PluginId::new(id.clone()).map_err(|e| invalid(name, format!("{id}: {e}")))?;
            let req =
                VersionReq::parse(&req).map_err(|e| invalid(name, format!("plugins.{id}: {e}")))?;
            plugins.insert(id, req);
        }
        let mut sources = BTreeMap::new();
        for (id, source) in raw.sources {
            let id = PluginId::new(id.clone()).map_err(|e| invalid(name, format!("{id}: {e}")))?;
            sources.insert(id, base.join(source.path));
        }
        let mut settings = toml_to_json(raw.settings);
        // The theme of `[resources]` is the `workbench.theme` setting of the kit.
        if let Some(theme) = &raw.resources.theme {
            settings
                .entry("workbench.theme".to_owned())
                .or_insert_with(|| Value::String(theme.clone()));
        }
        let _ = raw.resource;
        Ok(Self {
            id: raw.kit.id,
            name: raw.kit.name,
            version,
            description: raw.kit.description,
            exclusive: raw.kit.exclusive,
            kumiko,
            plugins,
            sources,
            resources: raw.resources,
            settings,
        })
    }

    pub fn load(path: &Path) -> Result<Self, KitError> {
        let name = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| invalid(&name, e))?;
        Self::parse(&text, path.parent().unwrap_or(Path::new(".")), &name)
    }

    /// What applying the kit changes, given what is installed.
    pub fn plan(&self, registry: &PluginRegistry, bundled: Option<&Path>) -> Plan {
        let mut plan = Plan::default();
        for (id, req) in &self.plugins {
            match registry.get(id) {
                None => match self.source_of(id, bundled) {
                    Some(source) => plan.install.push((id.clone(), source)),
                    None => plan.missing.push(id.clone()),
                },
                Some(entry) => {
                    let version = &entry.manifest().plugin.version;
                    if !req.matches(version) {
                        plan.mismatched
                            .push((id.clone(), version.clone(), req.clone()));
                    }
                    if entry.status == PluginStatus::Disabled {
                        plan.enable.push(id.clone());
                    }
                }
            }
        }
        if self.exclusive {
            for entry in registry.entries() {
                let id = entry.manifest().id();
                if !self.plugins.contains_key(id) && entry.status != PluginStatus::Disabled {
                    plan.disable.push(id.clone());
                }
            }
        }
        plan
    }

    /// The folder a missing plugin is installed from: the kit's source, else the bundled
    /// plugins of the distribution (`<bundled>/<id>`).
    fn source_of(&self, id: &PluginId, bundled: Option<&Path>) -> Option<PathBuf> {
        let declared = self
            .sources
            .get(id)
            .filter(|p| p.join("plugin.toml").is_file());
        declared.cloned().or_else(|| {
            bundled
                .map(|dir| dir.join(id.as_str()))
                .filter(|p| p.join("plugin.toml").is_file())
        })
    }

    /// The kit as a file again (the applied kit is kept this way; sources are not kept, the
    /// plugins are installed by then).
    pub fn to_toml(&self) -> String {
        let mut out = format!(
            "[kit]
id = {:?}
name = {:?}
version = \"{}\"
description = {:?}
exclusive = {}
",
            self.id, self.name, self.version, self.description, self.exclusive
        );
        if let Some(req) = &self.kumiko {
            out.push_str(&format!(
                "kumiko = {:?}
",
                req.to_string()
            ));
        }
        out.push_str(
            "
[plugins]
",
        );
        for (id, req) in &self.plugins {
            out.push_str(&format!(
                "{:?} = {:?}
",
                id.as_str(),
                req.to_string()
            ));
        }
        if !self.settings.is_empty() {
            out.push_str(
                "
[settings]
",
            );
            for (key, value) in &self.settings {
                out.push_str(&format!(
                    "{key:?} = {}
",
                    json_to_toml(value)
                ));
            }
        }
        out
    }

    /// A snapshot of the current state as a kit: the enabled plugins and the settings given.
    pub fn export(
        id: &str,
        name: &str,
        registry: &PluginRegistry,
        settings: &Map<String, Value>,
    ) -> String {
        let mut out = format!(
            "[kit]\nid = {id:?}\nname = {name:?}\nversion = \"0.1.0\"\nexclusive = true\n\n[plugins]\n"
        );
        let mut plugins: Vec<(&PluginId, &Version)> = registry
            .entries()
            .filter(|e| e.status != PluginStatus::Disabled)
            .map(|e| (e.manifest().id(), &e.manifest().plugin.version))
            .collect();
        plugins.sort();
        for (id, version) in plugins {
            out.push_str(&format!("{:?} = \"^{version}\"\n", id.as_str()));
        }
        if !settings.is_empty() {
            out.push_str("\n[settings]\n");
            for (key, value) in settings {
                out.push_str(&format!("{key:?} = {}\n", json_to_toml(value)));
            }
        }
        out
    }
}

/// What applying a kit does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Plugins to install, from where.
    pub install: Vec<(PluginId, PathBuf)>,
    pub enable: Vec<PluginId>,
    pub disable: Vec<PluginId>,
    /// Plugins the kit wants that are neither installed nor have a source.
    pub missing: Vec<PluginId>,
    /// Installed plugins whose version the kit does not accept.
    pub mismatched: Vec<(PluginId, Version, VersionReq)>,
}

impl Plan {
    /// Nothing to do (a second application).
    pub fn is_empty(&self) -> bool {
        self.install.is_empty() && self.enable.is_empty() && self.disable.is_empty()
    }

    /// The plan in words, for the UI and the CLI: one line per change.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(self.install.iter().map(|(id, _)| format!("install {id}")));
        lines.extend(self.enable.iter().map(|id| format!("enable {id}")));
        lines.extend(self.disable.iter().map(|id| format!("disable {id}")));
        lines.extend(
            self.missing
                .iter()
                .map(|id| format!("missing {id}: not installed, no source")),
        );
        lines.extend(self.mismatched.iter().map(|(id, have, want)| {
            format!("version {id}: {have} installed, the kit wants {want}")
        }));
        lines
    }
}

fn toml_to_json(table: toml::Table) -> Map<String, Value> {
    table.into_iter().map(|(k, v)| (k, toml_value(v))).collect()
}

fn toml_value(value: toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => Value::from(f),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.into_iter().map(toml_value).collect()),
        toml::Value::Table(t) => Value::Object(toml_to_json(t)),
    }
}

fn json_to_toml(value: &Value) -> String {
    match value {
        Value::String(s) => format!("{s:?}"),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(json_to_toml).collect::<Vec<_>>().join(", ")
        ),
        Value::Null => "\"\"".to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIT: &str = r#"
[kit]
id = "kumiko.kit.java"
name = "Java Developer"
version = "0.1.0"
kumiko = "^0.1"
description = "Java + Maven"
exclusive = true

[plugins]
"kumiko.java" = "^0.1"
"kumiko.git" = "^0.1"

[sources]
"kumiko.java" = { path = "../plugins/java" }

[resources]
theme = "kumiko.theme.hinoki-dark"

[settings]
"editor.fontSize" = 14
"#;

    #[test]
    fn parses_the_example_of_the_spec() {
        let kit = Kit::parse(KIT, Path::new("/kits"), "java.kit.toml").unwrap();
        assert_eq!(kit.id, "kumiko.kit.java");
        assert!(kit.exclusive);
        assert_eq!(kit.plugins.len(), 2);
        assert_eq!(
            kit.sources[&PluginId::new("kumiko.java").unwrap()],
            Path::new("/kits/../plugins/java")
        );
        assert_eq!(kit.settings["editor.fontSize"], 14);
        assert_eq!(kit.settings["workbench.theme"], "kumiko.theme.hinoki-dark");
    }

    #[test]
    fn mistakes_name_the_file_and_the_cause() {
        for (bad, cause) in [
            (
                "[kit]\nid = \"a\"\nname = \"A\"\nversion = \"x\"\n",
                "kit.version",
            ),
            (
                "[kit]\nid = \"a\"\nname = \"A\"\nversion = \"1.0.0\"\n[plugins]\n\"nodot\" = \"^1\"\n",
                "nodot",
            ),
            (
                "[kit]\nid = \"a\"\nname = \"A\"\nversion = \"1.0.0\"\n[plugins]\n\"a.b\" = \"???\"\n",
                "plugins.a.b",
            ),
            ("[kit]\nid = \"a\"\n", "name"),
        ] {
            let error = Kit::parse(bad, Path::new("."), "x.kit.toml")
                .unwrap_err()
                .to_string();
            assert!(error.starts_with("x.kit.toml"), "{error}");
            assert!(error.contains(cause), "{error}");
        }
    }

    #[test]
    fn an_empty_plan_is_empty() {
        assert!(Plan::default().is_empty());
        let plan = Plan {
            missing: vec![PluginId::new("a.b").unwrap()],
            ..Plan::default()
        };
        assert!(plan.is_empty(), "missing plugins are reported, not changes");
        assert_eq!(plan.lines(), ["missing a.b: not installed, no source"]);
    }
}
