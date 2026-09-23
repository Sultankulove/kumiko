//! `plugin.toml`: everything a plugin adds to the IDE, declared before any of its code runs
//! (spec, section 13).
//!
//! Parsing is strict: unknown keys are errors, so a typo in a manifest is reported instead of
//! silently dropping a contribution. Checks that need other plugins (dependencies, required
//! interfaces, id conflicts) belong to installation, not to this module.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use kumiko_service_api::when::WhenExpr;
use kumiko_types::{CommandId, InterfaceId, PluginId};
use semver::{Version, VersionReq};
use serde::Deserialize;

/// File name of the manifest inside a plugin directory.
pub const MANIFEST_FILE: &str = "plugin.toml";

/// A manifest that cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Not valid TOML, or a value of the wrong shape; the message carries the location.
    #[error("{0}")]
    Syntax(#[from] toml::de::Error),
    /// Well-formed, but breaks a rule that serde cannot express.
    #[error("{field}: {reason}")]
    Invalid { field: String, reason: String },
}

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> ManifestError {
    ManifestError::Invalid {
        field: field.into(),
        reason: reason.into(),
    }
}

/// A parsed and validated `plugin.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub plugin: PluginSection,
    /// Other plugins this one needs, with version ranges; never crates.
    #[serde(default)]
    pub dependencies: BTreeMap<PluginId, VersionReq>,
    #[serde(default)]
    pub provides: Vec<Provides>,
    #[serde(default)]
    pub consumes: Vec<Consumes>,
    #[serde(default)]
    pub contributes: Contributes,
    #[serde(default)]
    pub l10n: Option<L10n>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSection {
    pub id: PluginId,
    /// Display name, usually a `%key%` resolved through the plugin's l10n.
    pub name: String,
    pub version: Version,
    /// KPP versions the plugin works with.
    pub api: VersionReq,
    pub kind: PluginKind,
    /// Executable or component, relative to the plugin directory; absent for data plugins.
    #[serde(default)]
    pub entry: Option<RelativePath>,
    #[serde(default)]
    pub permissions: BTreeSet<Permission>,
    #[serde(default)]
    pub activation: Vec<ActivationEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    /// A WebAssembly component run in-process by the WASM host.
    Wasm,
    /// A separate process speaking KPP over stdio.
    Process,
    /// Contributions only, no code.
    Data,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub enum Permission {
    #[serde(rename = "fs.workspace")]
    FsWorkspace,
    #[serde(rename = "fs.home")]
    FsHome,
    #[serde(rename = "process.spawn")]
    ProcessSpawn,
    #[serde(rename = "network")]
    Network,
    #[serde(rename = "ui")]
    Ui,
}

/// When the host starts the plugin's code.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum ActivationEvent {
    /// A workspace folder contains a file matching the glob.
    WorkspaceContains(String),
    /// A document with this language id is opened.
    OnLanguage(String),
    /// A command matching the pattern is executed; a trailing `*` matches any suffix.
    OnCommand(String),
    /// Another plugin calls this interface (section 14).
    OnInterface(InterfaceId),
}

impl ActivationEvent {
    /// Whether executing `command` triggers this event.
    pub fn matches_command(&self, command: &str) -> bool {
        match self {
            Self::OnCommand(pattern) => match pattern.strip_suffix('*') {
                Some(prefix) => command.starts_with(prefix),
                None => command == pattern,
            },
            _ => false,
        }
    }
}

impl TryFrom<String> for ActivationEvent {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        let Some((kind, argument)) = value.split_once(':') else {
            return Err(format!("activation event `{value}` has no `kind:` prefix"));
        };
        if argument.is_empty() || argument.chars().any(char::is_whitespace) {
            return Err(format!(
                "activation event `{value}` has an empty or blank argument"
            ));
        }
        match kind {
            "workspaceContains" => match globset::Glob::new(argument) {
                Ok(_) => Ok(Self::WorkspaceContains(argument.to_owned())),
                Err(error) => Err(format!("activation event `{value}`: {error}")),
            },
            "onLanguage" => Ok(Self::OnLanguage(argument.to_owned())),
            "onCommand" => {
                let fixed = argument.strip_suffix('*').unwrap_or(argument);
                let valid = fixed
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
                if valid {
                    Ok(Self::OnCommand(argument.to_owned()))
                } else {
                    Err(format!(
                        "activation event `{value}`: `*` is allowed only at the end"
                    ))
                }
            }
            "onInterface" => InterfaceId::new(argument)
                .map(Self::OnInterface)
                .map_err(|error| format!("activation event `{value}`: {error}")),
            _ => Err(format!("unknown activation event kind `{kind}`")),
        }
    }
}

/// A path inside the plugin directory: relative, without `..`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct RelativePath(String);

impl RelativePath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn resolve(&self, plugin_dir: &Path) -> PathBuf {
        plugin_dir.join(&self.0)
    }
}

impl TryFrom<String> for RelativePath {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        let inside = !value.is_empty()
            && Path::new(&value)
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if inside {
            Ok(Self(value))
        } else {
            Err(format!(
                "`{value}` must be a relative path inside the plugin directory"
            ))
        }
    }
}

/// `[[provides]]`: an interface this plugin implements (section 14).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provides {
    pub interface: InterfaceId,
    pub version: Version,
    #[serde(default)]
    pub schema: Option<RelativePath>,
}

/// `[[consumes]]`: an interface this plugin calls.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Consumes {
    pub interface: InterfaceId,
    pub version: VersionReq,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Contributes {
    pub commands: Vec<CommandContribution>,
    pub menus: Vec<MenuContribution>,
    pub keybindings: Vec<KeybindingContribution>,
    pub languages: Vec<LanguageContribution>,
    pub language_servers: Vec<LanguageServerContribution>,
    pub configuration: Option<ConfigurationContribution>,
    pub task_providers: Vec<TaskProviderContribution>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandContribution {
    pub id: CommandId,
    pub title: String,
    #[serde(default)]
    pub when: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuContribution {
    /// Menu identifier, e.g. `editor/context`.
    pub location: String,
    pub command: CommandId,
    #[serde(default)]
    pub when: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeybindingContribution {
    /// Key sequence in keymap syntax; parsed by the config service when the keymap is built.
    pub key: String,
    pub command: CommandId,
    #[serde(default)]
    pub when: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageContribution {
    pub id: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Id of an L3 grammar package.
    #[serde(default)]
    pub grammar: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageServerContribution {
    pub id: String,
    pub languages: Vec<String>,
    pub launch: LanguageServerLaunch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageServerLaunch {
    /// The command line is requested from the plugin with `lsp/resolveLaunch`.
    Dynamic,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationContribution {
    /// JSON Schema of the plugin's settings.
    pub schema: RelativePath,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProviderContribution {
    #[serde(rename = "type")]
    pub task_type: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct L10n {
    pub default: String,
    pub path: RelativePath,
}

impl Manifest {
    /// Parses and validates the text of a `plugin.toml`.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn id(&self) -> &PluginId {
        &self.plugin.id
    }

    /// Whether a host speaking KPP `version` can run this plugin.
    pub fn supports_api(&self, version: &Version) -> bool {
        self.plugin.api.matches(version)
    }

    pub fn has_permission(&self, permission: Permission) -> bool {
        self.plugin.permissions.contains(&permission)
    }

    /// Whether executing `command` should start the plugin.
    pub fn activates_on_command(&self, command: &str) -> bool {
        self.plugin
            .activation
            .iter()
            .any(|event| event.matches_command(command))
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let plugin = &self.plugin;
        if plugin.name.trim().is_empty() {
            return Err(invalid("plugin.name", "must not be empty"));
        }
        match (plugin.kind, &plugin.entry) {
            (PluginKind::Data, Some(_)) => {
                return Err(invalid("plugin.entry", "data plugins have no code"));
            }
            (PluginKind::Data, None) if !plugin.activation.is_empty() => {
                return Err(invalid(
                    "plugin.activation",
                    "data plugins have no code to activate",
                ));
            }
            (PluginKind::Wasm | PluginKind::Process, None) => {
                return Err(invalid(
                    "plugin.entry",
                    "required for wasm and process plugins",
                ));
            }
            (PluginKind::Wasm, Some(entry)) if !entry.as_str().ends_with(".wasm") => {
                return Err(invalid(
                    "plugin.entry",
                    "a wasm plugin's entry is a .wasm component",
                ));
            }
            _ => {}
        }
        if self.dependencies.contains_key(&plugin.id) {
            return Err(invalid("dependencies", "a plugin cannot depend on itself"));
        }
        unique(
            "provides",
            self.provides.iter().map(|p| p.interface.as_str()),
        )?;
        unique(
            "consumes",
            self.consumes.iter().map(|c| c.interface.as_str()),
        )?;

        let contributes = &self.contributes;
        unique(
            "contributes.commands",
            contributes.commands.iter().map(|c| c.id.as_str()),
        )?;
        unique(
            "contributes.languages",
            contributes.languages.iter().map(|l| l.id.as_str()),
        )?;
        unique(
            "contributes.languageServers",
            contributes.language_servers.iter().map(|s| s.id.as_str()),
        )?;
        let whens = contributes
            .commands
            .iter()
            .map(|c| ("contributes.commands", &c.when))
            .chain(
                contributes
                    .menus
                    .iter()
                    .map(|m| ("contributes.menus", &m.when)),
            )
            .chain(
                contributes
                    .keybindings
                    .iter()
                    .map(|k| ("contributes.keybindings", &k.when)),
            );
        for (field, when) in whens {
            if let Some(source) = when
                && let Err(error) = WhenExpr::parse(source)
            {
                return Err(invalid(field, format!("`{source}`: {error}")));
            }
        }
        if contributes
            .keybindings
            .iter()
            .any(|k| k.key.trim().is_empty())
        {
            return Err(invalid(
                "contributes.keybindings",
                "`key` must not be empty",
            ));
        }
        if let Some(language) = contributes
            .languages
            .iter()
            .find(|l| l.extensions.iter().any(|e| !e.starts_with('.')))
        {
            return Err(invalid(
                "contributes.languages",
                format!("extensions of `{}` must start with a dot", language.id),
            ));
        }
        if let Some(server) = contributes
            .language_servers
            .iter()
            .find(|s| s.languages.is_empty())
        {
            return Err(invalid(
                "contributes.languageServers",
                format!("`{}` serves no languages", server.id),
            ));
        }
        Ok(())
    }
}

fn unique<'a>(field: &str, values: impl Iterator<Item = &'a str>) -> Result<(), ManifestError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(invalid(field, format!("`{value}` is declared twice")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example from the spec, section 13.
    const JAVA: &str = r#"
[plugin]
id = "kumiko.java"
name = "%plugin.name%"
version = "0.1.0"
api = "^1.0"
kind = "process"
entry = "bin/kumiko-java"
permissions = ["fs.workspace", "process.spawn", "network"]
activation = [
  "workspaceContains:**/pom.xml",
  "workspaceContains:**/build.gradle*",
  "onLanguage:java",
  "onCommand:java.*",
]

[dependencies]

[[provides]]
interface = "kumiko.java.project"
version = "1.0.0"
schema = "interfaces/java.project.json"

[[consumes]]
interface = "kumiko.git.repository"
version = "^1"
optional = true

[[contributes.commands]]
id = "java.selectJdk"
title = "%command.selectJdk%"
when = "workspaceHasJava"

[[contributes.menus]]
location = "editor/context"
command = "java.organizeImports"
when = "editorLangId == java"

[[contributes.keybindings]]
key = "ctrl+shift+o"
command = "java.organizeImports"
when = "editorLangId == java"

[[contributes.languages]]
id = "java"
extensions = [".java"]
grammar = "kumiko.grammar.java"

[[contributes.languageServers]]
id = "jdtls"
languages = ["java"]
launch = "dynamic"

[contributes.configuration]
schema = "schema/config.json"

[[contributes.taskProviders]]
type = "maven"

[l10n]
default = "en"
path = "l10n/"
"#;

    fn with_plugin(plugin: &str, rest: &str) -> String {
        format!(
            "[plugin]\nid = \"test.plugin\"\nname = \"Test\"\nversion = \"1.2.3\"\napi = \"^0.1\"\n{plugin}\n{rest}"
        )
    }

    fn error(text: &str) -> String {
        match Manifest::parse(text) {
            Ok(_) => panic!("manifest accepted:\n{text}"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn spec_example_parses() {
        let manifest = Manifest::parse(JAVA).unwrap();
        assert_eq!(manifest.id().as_str(), "kumiko.java");
        assert_eq!(manifest.plugin.kind, PluginKind::Process);
        assert_eq!(
            manifest.plugin.entry.as_ref().unwrap().as_str(),
            "bin/kumiko-java"
        );
        assert!(manifest.has_permission(Permission::ProcessSpawn));
        assert!(!manifest.has_permission(Permission::Ui));
        assert_eq!(manifest.plugin.activation.len(), 4);
        assert_eq!(
            manifest.plugin.activation[2],
            ActivationEvent::OnLanguage("java".into())
        );
        assert!(manifest.consumes[0].optional);
        assert_eq!(manifest.provides[0].version, Version::new(1, 0, 0));
        assert_eq!(
            manifest.contributes.commands[0].id.as_str(),
            "java.selectJdk"
        );
        assert_eq!(
            manifest.contributes.language_servers[0].launch,
            LanguageServerLaunch::Dynamic
        );
        assert_eq!(manifest.contributes.task_providers[0].task_type, "maven");
        assert_eq!(manifest.l10n.as_ref().unwrap().default, "en");
        assert!(manifest.supports_api(&Version::new(1, 4, 0)));
        assert!(!manifest.supports_api(&Version::new(0, 1, 0)));
        assert!(!manifest.supports_api(&Version::new(2, 0, 0)));
    }

    #[test]
    fn command_activation_patterns() {
        let manifest = Manifest::parse(JAVA).unwrap();
        assert!(manifest.activates_on_command("java.selectJdk"));
        assert!(!manifest.activates_on_command("javascript.run"));
        let exact = ActivationEvent::try_from("onCommand:echo.say".to_owned()).unwrap();
        assert!(exact.matches_command("echo.say"));
        assert!(!exact.matches_command("echo.sayMore"));
    }

    #[test]
    fn minimal_plugins() {
        let data = Manifest::parse(&with_plugin("kind = \"data\"", "")).unwrap();
        assert!(data.plugin.entry.is_none() && data.plugin.permissions.is_empty());
        let wasm =
            Manifest::parse(&with_plugin("kind = \"wasm\"\nentry = \"plugin.wasm\"", "")).unwrap();
        assert_eq!(wasm.plugin.kind, PluginKind::Wasm);
        assert!(wasm.supports_api(&Version::new(0, 1, 5)));
        assert!(!wasm.supports_api(&Version::new(0, 2, 0)));
    }

    #[test]
    fn rejects_malformed_manifests() {
        let process = "kind = \"process\"\nentry = \"bin/p\"";
        let cases = [
            (with_plugin(process, "[plugin2]"), "unknown field"),
            (
                with_plugin(&format!("{process}\nactivaton = []"), ""),
                "unknown field",
            ),
            (
                with_plugin(process, "").replace("test.plugin", "plugin"),
                "plugin id",
            ),
            (with_plugin(process, "").replace("1.2.3", "1.2"), "version"),
            (with_plugin("kind = \"native\"", ""), "unknown variant"),
            (
                with_plugin(&format!("{process}\npermissions = [\"root\"]"), ""),
                "unknown variant",
            ),
            (
                with_plugin(process, "").replace("\"Test\"", "\" \""),
                "plugin.name",
            ),
            (
                with_plugin("kind = \"process\"", ""),
                "required for wasm and process",
            ),
            (
                with_plugin("kind = \"data\"\nentry = \"x\"", ""),
                "data plugins have no code",
            ),
            (
                with_plugin("kind = \"data\"\nactivation = [\"onLanguage:java\"]", ""),
                "no code to activate",
            ),
            (
                with_plugin("kind = \"wasm\"\nentry = \"plugin.so\"", ""),
                ".wasm component",
            ),
            (
                with_plugin("kind = \"process\"\nentry = \"../evil\"", ""),
                "relative path",
            ),
            (
                with_plugin("kind = \"process\"\nentry = \"/usr/bin/sh\"", ""),
                "relative path",
            ),
            (
                with_plugin("kind = \"process\"\nentry = \"\"", ""),
                "relative path",
            ),
        ];
        for (text, expected) in cases {
            let message = error(&text);
            assert!(
                message.contains(expected),
                "expected `{expected}` in:\n{message}\n---\n{text}"
            );
        }
    }

    #[test]
    fn rejects_bad_activation_events() {
        for (event, expected) in [
            ("java", "no `kind:` prefix"),
            ("onLanguage:", "empty or blank"),
            ("onStartup:now", "unknown activation event kind"),
            ("workspaceContains:**/[pom", "workspaceContains"),
            ("onCommand:java.*.run", "only at the end"),
            ("onInterface:git", "interface id"),
        ] {
            let text = with_plugin(
                &format!("kind = \"process\"\nentry = \"p\"\nactivation = [\"{event}\"]"),
                "",
            );
            let message = error(&text);
            assert!(
                message.contains(expected),
                "`{event}`: expected `{expected}` in {message}"
            );
        }
    }

    #[test]
    fn rejects_inconsistent_sections() {
        let process = "kind = \"process\"\nentry = \"p\"";
        let command = "[[contributes.commands]]\nid = \"test.run\"\ntitle = \"Run\"\n";
        let provides = "[[provides]]\ninterface = \"test.api\"\nversion = \"1.0.0\"\n";
        let cases = [
            (
                "[dependencies]\n\"test.plugin\" = \"^1\"".to_owned(),
                "depend on itself",
            ),
            (
                "[dependencies]\n\"other.plugin\" = \"one\"".to_owned(),
                "major version number",
            ),
            (
                format!("{provides}{provides}"),
                "`test.api` is declared twice",
            ),
            (
                format!("{command}{command}"),
                "`test.run` is declared twice",
            ),
            (format!("{command}when = \"a &&\""), "contributes.commands"),
            (
                "[[contributes.keybindings]]\nkey = \" \"\ncommand = \"test.run\"".to_owned(),
                "`key` must not be empty",
            ),
            (
                "[[contributes.languages]]\nid = \"x\"\nextensions = [\"x\"]".to_owned(),
                "must start with a dot",
            ),
            (
                "[[contributes.languageServers]]\nid = \"s\"\nlanguages = []\nlaunch = \"dynamic\""
                    .to_owned(),
                "serves no languages",
            ),
            (
                "[contributes.configuration]\nschema = \"../x.json\"".to_owned(),
                "relative path",
            ),
        ];
        for (rest, expected) in cases {
            let text = with_plugin(process, &rest);
            let message = error(&text);
            assert!(
                message.contains(expected),
                "expected `{expected}` in:\n{message}\n---\n{text}"
            );
        }
    }
}
