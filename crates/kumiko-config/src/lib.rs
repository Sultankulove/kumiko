//! Config service: layered settings, plugin schemas, keymap (spec, section 22; ADR-007).
//!
//! Settings are flat namespaced keys (`editor.fontSize`) described by JSON Schema. Layers,
//! the last one winning: schema defaults → kit → user file → workspace file → language
//! overrides (`[language.<id>]` of the user file, then of the workspace file).
//!
//! A value that does not match its schema is skipped (the next lower layer applies) and
//! reported in [`ConfigService::problems`]; the IDE never fails on bad settings. Keys
//! without a schema (e.g. of a plugin not loaded yet) are passed through unchecked. The
//! top-level `language` table is reserved for overrides.

pub mod keymap;
mod schema;
mod toml;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde_json::{Map, Value};
use toml_edit::DocumentMut;

use kumiko_protocol::events::ConfigChanged;
use kumiko_protocol::methods::config::ConfigTarget;
use kumiko_service_api::{Event, EventSink};

pub use keymap::{Keymap, KeymapSource};
pub use schema::{PropertySchema, SchemaError};

const LANGUAGE_TABLE: &str = "language";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("no {0:?} settings file is configured")]
    NoFile(ConfigTarget),
    #[error("invalid value for `{key}`: {message}")]
    Invalid { key: String, message: String },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error("`{key}` is already described by `{owner}`")]
    DuplicateKey { key: String, owner: String },
}

/// Something wrong with the settings, shown in Problems.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigProblem {
    /// File path or layer name.
    pub source: String,
    pub key: Option<String>,
    pub message: String,
}

#[derive(Default)]
struct FileLayer {
    path: Option<PathBuf>,
    tree: Map<String, Value>,
    parse_problem: Option<ConfigProblem>,
}

#[derive(Default)]
struct State {
    schemas: BTreeMap<String, (String, PropertySchema)>,
    kit: Map<String, Value>,
    user: FileLayer,
    workspace: FileLayer,
    keymap: Keymap,
    keymap_problems: Vec<ConfigProblem>,
}

/// A layer flattened for lookups.
struct Flat {
    source: String,
    values: Map<String, Value>,
}

impl State {
    fn keep_object(&self, key: &str) -> bool {
        self.schemas.get(key).is_some_and(|(_, s)| s.is_object())
    }

    fn flatten(&self, tree: &Map<String, Value>) -> Map<String, Value> {
        let mut out = Map::new();
        let keep = |key: &str| self.keep_object(key);
        toml::flatten(tree, "", &keep, &mut out);
        out
    }

    /// Layers from lowest to highest for an optional language.
    fn layers(&self, language: Option<&str>) -> Vec<Flat> {
        let defaults = self
            .schemas
            .iter()
            .filter_map(|(k, (_, s))| Some((k.clone(), s.default_value()?.clone())))
            .collect();
        let mut layers = vec![
            Flat {
                source: "defaults".into(),
                values: defaults,
            },
            Flat {
                source: "kit".into(),
                values: self.kit.clone(),
            },
        ];
        for file in [&self.user, &self.workspace] {
            let mut base = file.tree.clone();
            base.remove(LANGUAGE_TABLE);
            layers.push(Flat {
                source: file_name(file),
                values: self.flatten(&base),
            });
        }
        if let Some(language) = language {
            for file in [&self.user, &self.workspace] {
                let tree = file
                    .tree
                    .get(LANGUAGE_TABLE)
                    .and_then(|l| l.get(language))
                    .and_then(Value::as_object);
                if let Some(tree) = tree {
                    layers.push(Flat {
                        source: format!("{} [language.{language}]", file_name(file)),
                        values: self.flatten(tree),
                    });
                }
            }
        }
        layers
    }

    fn valid(&self, key: &str, value: &Value) -> Result<(), String> {
        match self.schemas.get(key) {
            Some((_, schema)) => schema.validate(value),
            None => Ok(()),
        }
    }

    fn get(&self, key: &str, language: Option<&str>) -> Option<Value> {
        self.layers(language)
            .into_iter()
            .rev()
            .filter_map(|layer| layer.values.get(key).cloned())
            .find(|value| self.valid(key, value).is_ok())
    }

    fn languages(&self) -> BTreeSet<String> {
        [&self.user, &self.workspace]
            .iter()
            .filter_map(|f| f.tree.get(LANGUAGE_TABLE).and_then(Value::as_object))
            .flat_map(|l| l.keys().cloned())
            .collect()
    }

    /// Every effective value, keyed by `key` and `[language]key`, for change detection.
    fn effective(&self) -> BTreeMap<String, Value> {
        let mut all = BTreeMap::new();
        let mut scopes = vec![None];
        scopes.extend(self.languages().into_iter().map(Some));
        for language in scopes {
            let keys: BTreeSet<String> = self
                .layers(language.as_deref())
                .into_iter()
                .flat_map(|l| l.values.into_iter().map(|(k, _)| k))
                .collect();
            for key in keys {
                if let Some(value) = self.get(&key, language.as_deref()) {
                    let scoped = match &language {
                        Some(language) => format!("[{language}]{key}"),
                        None => key,
                    };
                    all.insert(scoped, value);
                }
            }
        }
        all
    }

    fn problems(&self) -> Vec<ConfigProblem> {
        let mut problems: Vec<ConfigProblem> = [&self.user, &self.workspace]
            .iter()
            .filter_map(|f| f.parse_problem.clone())
            .collect();
        let mut scopes = vec![None];
        scopes.extend(self.languages().into_iter().map(Some));
        for language in scopes {
            for layer in self.layers(language.as_deref()).into_iter().skip(1) {
                if language.is_some() && !layer.source.contains("[language.") {
                    continue;
                }
                for (key, value) in &layer.values {
                    if let Err(message) = self.valid(key, value) {
                        problems.push(ConfigProblem {
                            source: layer.source.clone(),
                            key: Some(key.clone()),
                            message: format!("{message}; using the value of a lower layer"),
                        });
                    }
                }
            }
        }
        problems.extend(self.keymap_problems.iter().cloned());
        problems
    }
}

fn file_name(file: &FileLayer) -> String {
    file.path
        .as_ref()
        .map_or_else(|| "settings".into(), |p| p.display().to_string())
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Reads a settings file; a missing file is an empty layer.
fn load_file(path: &Path) -> Result<Map<String, Value>, ConfigProblem> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => {
            return Err(ConfigProblem {
                source: path.display().to_string(),
                key: None,
                message: error.to_string(),
            });
        }
    };
    let document: DocumentMut =
        text.parse()
            .map_err(|error: toml_edit::TomlError| ConfigProblem {
                source: path.display().to_string(),
                key: None,
                message: format!("{error}; keeping the previous settings of this file"),
            })?;
    match toml::to_json(document.as_item()) {
        Value::Object(tree) => Ok(tree),
        _ => Ok(Map::new()),
    }
}

pub struct ConfigService {
    events: Arc<dyn EventSink>,
    state: RwLock<State>,
}

impl ConfigService {
    pub fn new(events: Arc<dyn EventSink>) -> Self {
        Self {
            events,
            state: RwLock::default(),
        }
    }

    /// Runs `change`, then publishes `config.changed` for keys whose value changed.
    fn changing(&self, change: impl FnOnce(&mut State)) -> Vec<String> {
        let mut state = write(&self.state);
        let before = state.effective();
        change(&mut state);
        let after = state.effective();
        drop(state);

        // `[rust]editor.tabSize` → `editor.tabSize`
        let unscoped = |key: &String| match key.strip_prefix('[').and_then(|r| r.split_once(']')) {
            Some((_, key)) => key.to_owned(),
            None => key.clone(),
        };
        // A language without overrides sees the unscoped value, so compare with that.
        let value = |map: &BTreeMap<String, Value>, key: &String| {
            map.get(key).or_else(|| map.get(&unscoped(key))).cloned()
        };
        let changed: BTreeSet<String> = before
            .keys()
            .chain(after.keys())
            .filter(|key| value(&before, key) != value(&after, key))
            .map(unscoped)
            .collect();
        let keys: Vec<String> = changed.into_iter().collect();
        if !keys.is_empty() {
            self.events
                .publish(Event::core(&ConfigChanged { keys: keys.clone() }));
        }
        keys
    }

    /// Adds the settings described by `schema` (`{"properties": {...}}`) for an owner.
    pub fn register_schema(&self, owner: &str, schema: &Value) -> Result<(), ConfigError> {
        let properties = schema::parse_properties(schema)?;
        {
            let state = read(&self.state);
            for (key, _) in &properties {
                if let Some((other, _)) = state.schemas.get(key)
                    && other != owner
                {
                    return Err(ConfigError::DuplicateKey {
                        key: key.clone(),
                        owner: other.clone(),
                    });
                }
            }
        }
        self.changing(|state| {
            for (key, property) in properties {
                state.schemas.insert(key, (owner.to_owned(), property));
            }
        });
        Ok(())
    }

    pub fn unregister_schema(&self, owner: &str) {
        self.changing(|state| state.schemas.retain(|_, (o, _)| o != owner));
    }

    pub fn schema(&self, key: &str) -> Option<PropertySchema> {
        read(&self.state).schemas.get(key).map(|(_, s)| s.clone())
    }

    /// Sets the settings of the active kit (flat or nested keys).
    pub fn set_kit(&self, values: Map<String, Value>) {
        self.changing(|state| {
            let flat = state.flatten(&values);
            state.kit = flat;
        });
    }

    pub fn set_user_file(&self, path: Option<PathBuf>) -> Vec<String> {
        self.changing(|state| {
            state.user.path = path;
            reload_layer(&mut state.user);
        })
    }

    pub fn set_workspace_file(&self, path: Option<PathBuf>) -> Vec<String> {
        self.changing(|state| {
            state.workspace.path = path;
            reload_layer(&mut state.workspace);
        })
    }

    /// Settings and keymap files the service reads; watch them and call [`Self::reload`].
    pub fn files(&self) -> Vec<PathBuf> {
        let state = read(&self.state);
        [&state.user.path, &state.workspace.path]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }

    /// Re-reads the settings files; returns (and publishes) the changed keys.
    pub fn reload(&self) -> Vec<String> {
        self.changing(|state| {
            reload_layer(&mut state.user);
            reload_layer(&mut state.workspace);
        })
    }

    /// Effective value of a setting for an optional language.
    pub fn get(&self, key: &str, language: Option<&str>) -> Option<Value> {
        read(&self.state).get(key, language)
    }

    /// Effective values under `prefix.`, keyed relative to the prefix.
    pub fn section(&self, prefix: &str, language: Option<&str>) -> Map<String, Value> {
        let state = read(&self.state);
        let dotted = format!("{prefix}.");
        let keys: BTreeSet<String> = state
            .layers(language)
            .into_iter()
            .flat_map(|l| l.values.into_iter().map(|(k, _)| k))
            .filter(|k| k.starts_with(&dotted))
            .collect();
        keys.into_iter()
            .filter_map(|key| {
                let value = state.get(&key, language)?;
                Some((key[dotted.len()..].to_owned(), value))
            })
            .collect()
    }

    /// Writes a setting to the user or workspace file, keeping the file's comments;
    /// `null` removes it.
    pub fn update(&self, key: &str, value: Value, target: ConfigTarget) -> Result<(), ConfigError> {
        let path = {
            let state = read(&self.state);
            if !value.is_null() {
                state
                    .valid(key, &value)
                    .map_err(|message| ConfigError::Invalid {
                        key: key.to_owned(),
                        message,
                    })?;
            }
            let file = match target {
                ConfigTarget::User => &state.user,
                ConfigTarget::Workspace => &state.workspace,
            };
            file.path.clone().ok_or(ConfigError::NoFile(target))?
        };
        let io = |e: std::io::Error| ConfigError::Io {
            path: path.clone(),
            message: e.to_string(),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(io(e)),
        };
        let mut document: DocumentMut =
            text.parse()
                .map_err(|e: toml_edit::TomlError| ConfigError::Parse {
                    path: path.clone(),
                    message: e.to_string(),
                })?;
        toml::set(&mut document, key, toml::from_json(&value));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        std::fs::write(&path, document.to_string()).map_err(io)?;
        self.reload();
        Ok(())
    }

    pub fn problems(&self) -> Vec<ConfigProblem> {
        read(&self.state).problems()
    }

    /// Replaces the bindings of one keymap source.
    pub fn set_keymap_source(&self, source: KeymapSource, bindings: Vec<keymap::Keybinding>) {
        write(&self.state).keymap.set_source(source, bindings);
    }

    /// Loads bindings of a source from a keymap file; a missing file clears the source.
    pub fn load_keymap_file(&self, source: KeymapSource, path: &Path) {
        let (bindings, problems) = match std::fs::read_to_string(path) {
            Ok(text) => keymap::parse_keymap(&text, source),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), Vec::new()),
            Err(e) => (Vec::new(), vec![e.to_string()]),
        };
        let mut state = write(&self.state);
        state.keymap.set_source(source, bindings);
        let origin = path.display().to_string();
        state.keymap_problems.retain(|p| p.source != origin);
        state
            .keymap_problems
            .extend(problems.into_iter().map(|message| ConfigProblem {
                source: origin.clone(),
                key: None,
                message,
            }));
    }

    pub fn keymap(&self) -> Keymap {
        read(&self.state).keymap.clone()
    }
}

fn reload_layer(layer: &mut FileLayer) {
    let Some(path) = &layer.path else {
        layer.tree = Map::new();
        layer.parse_problem = None;
        return;
    };
    match load_file(path) {
        Ok(tree) => {
            layer.tree = tree;
            layer.parse_problem = None;
        }
        Err(problem) => layer.parse_problem = Some(problem),
    }
}

#[cfg(test)]
mod tests;
