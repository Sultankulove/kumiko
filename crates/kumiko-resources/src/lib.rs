//! Resources service: registry of L3 data packages (spec, section 21; ADR-015).
//!
//! A package is a directory with `resource.toml`:
//!
//! ```toml
//! [resource]
//! id = "kumiko.theme.hinoki-dark"
//! kind = "theme"      # grammar | theme | icons | keymap | snippets | l10n | kit
//! version = "0.1.0"
//! name = "%theme.hinokiDark%"
//! ```
//!
//! plus tables of its kind in the same file (`[theme]`, `[colors]`, `[syntax]` for themes;
//! `[l10n]` and `[strings]` for translations; `[[binding]]` for keymaps). Packages from
//! later roots (user data) replace packages with the same id from earlier ones (built-in).

mod theme;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde_json::Value;
use toml_edit::DocumentMut;

pub use theme::{Color, SyntaxStyle, Theme, ThemeBase};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceKind {
    Grammar,
    Theme,
    Icons,
    Keymap,
    Snippets,
    L10n,
    Kit,
}

impl ResourceKind {
    fn parse(kind: &str) -> Option<Self> {
        Some(match kind {
            "grammar" => Self::Grammar,
            "theme" => Self::Theme,
            "icons" => Self::Icons,
            "keymap" => Self::Keymap,
            "snippets" => Self::Snippets,
            "l10n" => Self::L10n,
            "kit" => Self::Kit,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResourcePackage {
    pub id: String,
    pub kind: ResourceKind,
    pub version: String,
    pub name: Option<String>,
    pub dir: PathBuf,
    /// The whole `resource.toml` as JSON.
    pub manifest: Value,
}

impl ResourcePackage {
    pub fn manifest_path(&self) -> PathBuf {
        self.dir.join("resource.toml")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResourceProblem {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Default)]
struct State {
    packages: BTreeMap<String, ResourcePackage>,
    problems: Vec<ResourceProblem>,
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// TOML → JSON for manifests.
fn to_json(item: &toml_edit::Item) -> Value {
    fn value(v: &toml_edit::Value) -> Value {
        match v {
            toml_edit::Value::String(s) => Value::String(s.value().clone()),
            toml_edit::Value::Integer(i) => Value::from(*i.value()),
            toml_edit::Value::Float(f) => Value::from(*f.value()),
            toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
            toml_edit::Value::Datetime(d) => Value::String(d.value().to_string()),
            toml_edit::Value::Array(a) => Value::Array(a.iter().map(value).collect()),
            toml_edit::Value::InlineTable(t) => {
                Value::Object(t.iter().map(|(k, v)| (k.to_owned(), value(v))).collect())
            }
        }
    }
    match item {
        toml_edit::Item::None => Value::Null,
        toml_edit::Item::Value(v) => value(v),
        toml_edit::Item::Table(t) => {
            Value::Object(t.iter().map(|(k, v)| (k.to_owned(), to_json(v))).collect())
        }
        toml_edit::Item::ArrayOfTables(tables) => Value::Array(
            tables
                .iter()
                .map(|t| Value::Object(t.iter().map(|(k, v)| (k.to_owned(), to_json(v))).collect()))
                .collect(),
        ),
    }
}

fn load_package(dir: &Path) -> Result<ResourcePackage, String> {
    let text = std::fs::read_to_string(dir.join("resource.toml")).map_err(|e| e.to_string())?;
    let document: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let manifest = to_json(document.as_item());
    let header = manifest.get("resource").ok_or("missing [resource] table")?;
    let field = |name: &str| header.get(name).and_then(Value::as_str);
    let id = field("id").ok_or("missing `id`")?.to_owned();
    let kind = field("kind").ok_or("missing `kind`")?;
    let kind = ResourceKind::parse(kind).ok_or_else(|| format!("unknown kind `{kind}`"))?;
    Ok(ResourcePackage {
        id,
        kind,
        version: field("version").unwrap_or("0.0.0").to_owned(),
        name: field("name").map(str::to_owned),
        dir: dir.to_path_buf(),
        manifest,
    })
}

/// Finds package directories (containing `resource.toml`) up to two levels deep, so both
/// `<root>/<package>` and `<root>/<group>/<package>` layouts work.
fn package_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if dir.join("resource.toml").is_file() {
            dirs.push(dir);
            continue;
        }
        if depth == 2 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        children.sort();
        stack.extend(children.into_iter().rev().map(|p| (p, depth + 1)));
    }
    dirs
}

/// Registry of installed resource packages.
#[derive(Default)]
pub struct ResourceRegistry {
    state: RwLock<State>,
}

impl ResourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the registry with the packages found under `roots`, later roots winning.
    pub fn scan(&self, roots: &[PathBuf]) -> Vec<ResourceProblem> {
        let mut state = State::default();
        for root in roots {
            for dir in package_dirs(root) {
                match load_package(&dir) {
                    Ok(package) => {
                        state.problems.extend(validate(&package));
                        state.packages.insert(package.id.clone(), package);
                    }
                    Err(message) => state.problems.push(ResourceProblem {
                        path: dir.join("resource.toml"),
                        message,
                    }),
                }
            }
        }
        let problems = state.problems.clone();
        *write(&self.state) = state;
        problems
    }

    pub fn problems(&self) -> Vec<ResourceProblem> {
        read(&self.state).problems.clone()
    }

    pub fn get(&self, id: &str) -> Option<ResourcePackage> {
        read(&self.state).packages.get(id).cloned()
    }

    /// Packages of a kind, sorted by id.
    pub fn list(&self, kind: ResourceKind) -> Vec<ResourcePackage> {
        read(&self.state)
            .packages
            .values()
            .filter(|p| p.kind == kind)
            .cloned()
            .collect()
    }

    /// A theme with its `extends` chain resolved.
    pub fn theme(&self, id: &str) -> Option<Theme> {
        let state = read(&self.state);
        let mut chain = Vec::new();
        let mut next = Some(id.to_owned());
        while let Some(current) = next.take() {
            if chain.iter().any(|t: &Theme| t.id == current) {
                break; // a cycle; ignore the rest
            }
            let package = state.packages.get(&current)?;
            if package.kind != ResourceKind::Theme {
                return None;
            }
            let theme = Theme::from_manifest(&package.id, &package.manifest).ok()?;
            next = theme.extends.clone();
            chain.push(theme);
        }
        let mut chain = chain.into_iter();
        let mut theme = chain.next()?;
        for parent in chain {
            theme.inherit(&parent);
        }
        Some(theme)
    }

    /// The SVG file of an icon token (`toolwindow.project`): from the `preferred` icon pack
    /// when it has the token, else from the first pack, by id, that does.
    pub fn icon(&self, token: &str, preferred: Option<&str>) -> Option<PathBuf> {
        let state = read(&self.state);
        let file = |package: &ResourcePackage| {
            let name = package.manifest.get("icons")?.get(token)?.as_str()?;
            Some(package.dir.join(name))
        };
        let icons = |package: &&ResourcePackage| package.kind == ResourceKind::Icons;
        preferred
            .and_then(|id| state.packages.get(id))
            .filter(icons)
            .and_then(file)
            .or_else(|| state.packages.values().filter(icons).find_map(file))
    }

    /// Translation of `key` for `locale` (`ru-RU` → `ru` → `en`), or the key itself.
    pub fn localize(&self, key: &str, locale: &str) -> String {
        let state = read(&self.state);
        let language = locale.split(['-', '_']).next().unwrap_or(locale);
        for candidate in [locale, language, "en"] {
            for package in state
                .packages
                .values()
                .filter(|p| p.kind == ResourceKind::L10n)
            {
                let package_locale = package
                    .manifest
                    .pointer("/l10n/locale")
                    .and_then(Value::as_str);
                if package_locale != Some(candidate) {
                    continue;
                }
                if let Some(text) = package
                    .manifest
                    .get("strings")
                    .and_then(|s| s.get(key))
                    .and_then(Value::as_str)
                {
                    return text.to_owned();
                }
            }
        }
        key.to_owned()
    }

    /// Resolves `%key%` labels from manifests; other text is returned unchanged.
    pub fn label(&self, text: &str, locale: &str) -> String {
        match text.strip_prefix('%').and_then(|t| t.strip_suffix('%')) {
            Some(key) if !key.is_empty() => self.localize(key, locale),
            _ => text.to_owned(),
        }
    }
}

fn validate(package: &ResourcePackage) -> Vec<ResourceProblem> {
    let problem = |message: String| ResourceProblem {
        path: package.manifest_path(),
        message,
    };
    match package.kind {
        ResourceKind::Theme => match Theme::from_manifest(&package.id, &package.manifest) {
            Ok(theme) => theme.problems.into_iter().map(problem).collect(),
            Err(message) => vec![problem(message)],
        },
        ResourceKind::L10n => {
            let locale = package
                .manifest
                .pointer("/l10n/locale")
                .and_then(Value::as_str);
            let strings = package.manifest.get("strings").and_then(Value::as_object);
            let mut problems = Vec::new();
            if locale.is_none() {
                problems.push(problem("missing `[l10n] locale`".into()));
            }
            match strings {
                None => problems.push(problem("missing `[strings]` table".into())),
                Some(strings) => problems.extend(
                    strings
                        .iter()
                        .filter(|(_, v)| !v.is_string())
                        .map(|(k, _)| problem(format!("string `{k}` is not text"))),
                ),
            }
            problems
        }
        ResourceKind::Icons => match package.manifest.get("icons").and_then(Value::as_object) {
            None => vec![problem("missing `[icons]` table".into())],
            Some(icons) => icons
                .iter()
                .filter_map(|(token, file)| match file.as_str() {
                    None => Some(problem(format!("icon `{token}` is not a file name"))),
                    Some(name) if !package.dir.join(name).is_file() => {
                        Some(problem(format!("icon `{token}`: `{name}` not found")))
                    }
                    Some(_) => None,
                })
                .collect(),
        },
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
