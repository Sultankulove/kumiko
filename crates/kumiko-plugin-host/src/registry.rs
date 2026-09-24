//! Which installed plugins may run, and what they contribute (spec, sections 13 and 14).
//!
//! Resolution happens at startup, before any plugin code runs. A plugin is ready when it is
//! enabled, speaks a compatible KPP version, and every dependency and required interface is
//! satisfied by other ready plugins. Everything else stays installed with a reason.
//!
//! The registry does not touch other services: the core reads contributions from it and registers
//! them in commands, config and keymap.

use std::collections::{BTreeMap, BTreeSet};

use kumiko_types::{CommandId, InterfaceId, PluginId};
use semver::Version;

use crate::manifest::{
    CommandContribution, KeybindingContribution, LanguageContribution, Manifest, MenuContribution,
};
use crate::store::InstalledPlugin;

/// The KPP version this host speaks.
pub fn protocol_version() -> Version {
    Version::parse(kumiko_protocol::PROTOCOL_VERSION).unwrap_or_else(|_| Version::new(0, 0, 0))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginStatus {
    /// May be activated.
    Ready,
    /// Turned off by the user.
    Disabled,
    /// Installed, but cannot run; the reasons are shown to the user.
    Incompatible(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub plugin: InstalledPlugin,
    pub status: PluginStatus,
}

impl PluginEntry {
    pub fn manifest(&self) -> &Manifest {
        &self.plugin.manifest
    }

    pub fn is_ready(&self) -> bool {
        self.status == PluginStatus::Ready
    }
}

/// Ready plugins that differ between two registries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RegistryDiff {
    pub removed: Vec<PluginId>,
    pub added: Vec<PluginId>,
}

/// Installed plugins with their status and the contributions of the ready ones.
#[derive(Debug, Default)]
pub struct PluginRegistry {
    entries: BTreeMap<PluginId, PluginEntry>,
    commands: BTreeMap<CommandId, PluginId>,
    providers: BTreeMap<InterfaceId, Vec<PluginId>>,
    problems: Vec<String>,
}

impl PluginRegistry {
    /// Decides the status of every plugin against the host's KPP version.
    pub fn resolve(plugins: Vec<InstalledPlugin>, host_api: &Version) -> Self {
        let mut reasons: BTreeMap<PluginId, Vec<String>> = BTreeMap::new();
        let mut disabled = BTreeSet::new();
        let mut entries = BTreeMap::new();
        for plugin in plugins {
            let manifest = &plugin.manifest;
            let id = manifest.id().clone();
            if !plugin.enabled {
                disabled.insert(id.clone());
            } else if !manifest.supports_api(host_api) {
                reasons.entry(id.clone()).or_default().push(format!(
                    "requires KPP {}, the host speaks {host_api}",
                    manifest.plugin.api
                ));
            }
            entries.insert(id, plugin);
        }

        // Dependencies and interfaces are checked against the plugins that are still ready;
        // dropping one can break others, so repeat until nothing changes.
        loop {
            let ready = |id: &PluginId| !disabled.contains(id) && !reasons.contains_key(id);
            let mut found = Vec::new();
            for (id, plugin) in entries.iter().filter(|(id, _)| ready(id)) {
                let manifest = &plugin.manifest;
                for (dependency, range) in &manifest.dependencies {
                    let ok = entries.get(dependency).is_some_and(|d| {
                        ready(dependency) && range.matches(&d.manifest.plugin.version)
                    });
                    if !ok {
                        found.push((id.clone(), format!("needs plugin {dependency} {range}")));
                    }
                }
                for consumes in manifest.consumes.iter().filter(|c| !c.optional) {
                    let provided = entries.iter().any(|(provider, p)| {
                        provider != id
                            && ready(provider)
                            && p.manifest.provides.iter().any(|provides| {
                                provides.interface == consumes.interface
                                    && consumes.version.matches(&provides.version)
                            })
                    });
                    if !provided {
                        found.push((
                            id.clone(),
                            format!(
                                "needs interface {} {}",
                                consumes.interface, consumes.version
                            ),
                        ));
                    }
                }
            }
            if found.is_empty() {
                break;
            }
            for (id, reason) in found {
                reasons.entry(id).or_default().push(reason);
            }
        }

        let mut registry = Self::default();
        for (id, plugin) in entries {
            let status = if disabled.contains(&id) {
                PluginStatus::Disabled
            } else if let Some(reasons) = reasons.remove(&id) {
                PluginStatus::Incompatible(reasons)
            } else {
                PluginStatus::Ready
            };
            registry.entries.insert(id, PluginEntry { plugin, status });
        }
        registry.index();
        registry
    }

    /// Indexes commands and interface providers of ready plugins; the first plugin (by id) wins
    /// a command id, later ones are reported.
    fn index(&mut self) {
        for (id, entry) in self.entries.iter().filter(|(_, e)| e.is_ready()) {
            for command in &entry.manifest().contributes.commands {
                match self.commands.get(&command.id) {
                    Some(owner) => self.problems.push(format!(
                        "command {} of {id} is already contributed by {owner}",
                        command.id
                    )),
                    None => {
                        self.commands.insert(command.id.clone(), id.clone());
                    }
                }
            }
            for provides in &entry.manifest().provides {
                self.providers
                    .entry(provides.interface.clone())
                    .or_default()
                    .push(id.clone());
            }
        }
    }

    /// Ready plugins that stop being ready (`removed`) or become ready (`added`) when `self` is
    /// replaced by `new`. A plugin whose version or directory changed is in both lists.
    pub fn diff(&self, new: &PluginRegistry) -> RegistryDiff {
        let changed = |from: &PluginRegistry, to: &PluginRegistry| {
            from.ready()
                .filter(|entry| {
                    !to.get(entry.manifest().id()).is_some_and(|other| {
                        other.is_ready()
                            && other.manifest().plugin.version == entry.manifest().plugin.version
                            && other.plugin.dir == entry.plugin.dir
                    })
                })
                .map(|entry| entry.manifest().id().clone())
                .collect()
        };
        RegistryDiff {
            removed: changed(self, new),
            added: changed(new, self),
        }
    }

    pub fn get(&self, id: &PluginId) -> Option<&PluginEntry> {
        self.entries.get(id)
    }

    pub fn entries(&self) -> impl Iterator<Item = &PluginEntry> {
        self.entries.values()
    }

    pub fn ready(&self) -> impl Iterator<Item = &PluginEntry> {
        self.entries.values().filter(|e| e.is_ready())
    }

    /// Conflicts between ready plugins, for the Problems panel.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Ready plugins providing the interface, in id order.
    pub fn providers(&self, interface: &InterfaceId) -> &[PluginId] {
        self.providers.get(interface).map_or(&[], Vec::as_slice)
    }

    /// The ready plugin that has to run for `command`: its contributor, or else the first plugin
    /// whose `onCommand` activation matches.
    pub fn plugin_for_command(&self, command: &str) -> Option<&PluginId> {
        if let Ok(id) = CommandId::new(command)
            && let Some(owner) = self.commands.get(&id)
        {
            return Some(owner);
        }
        self.ready()
            .find(|e| e.manifest().activates_on_command(command))
            .map(|e| e.manifest().id())
    }

    /// Commands of ready plugins, without the ones lost in a conflict.
    pub fn commands(&self) -> impl Iterator<Item = (&PluginId, &CommandContribution)> {
        self.contributions(|m| &m.contributes.commands)
            .filter(|(id, command)| self.commands.get(&command.id) == Some(*id))
    }

    pub fn menus(&self) -> impl Iterator<Item = (&PluginId, &MenuContribution)> {
        self.contributions(|m| &m.contributes.menus)
    }

    pub fn keybindings(&self) -> impl Iterator<Item = (&PluginId, &KeybindingContribution)> {
        self.contributions(|m| &m.contributes.keybindings)
    }

    pub fn languages(&self) -> impl Iterator<Item = (&PluginId, &LanguageContribution)> {
        self.contributions(|m| &m.contributes.languages)
    }

    fn contributions<'a, T: 'a>(
        &'a self,
        field: impl Fn(&'a Manifest) -> &'a Vec<T> + 'a,
    ) -> impl Iterator<Item = (&'a PluginId, &'a T)> + 'a {
        self.ready().flat_map(move |e| {
            field(e.manifest())
                .iter()
                .map(move |item| (e.manifest().id(), item))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn plugin(id: &str, version: &str, extra: &str) -> InstalledPlugin {
        let text = format!(
            "[plugin]\nid = \"{id}\"\nname = \"{id}\"\nversion = \"{version}\"\napi = \"^0.1\"\nkind = \"data\"\n{extra}"
        );
        let manifest = Manifest::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        InstalledPlugin {
            manifest,
            dir: PathBuf::from(id),
            enabled: true,
        }
    }

    fn resolve(plugins: Vec<InstalledPlugin>) -> PluginRegistry {
        PluginRegistry::resolve(plugins, &Version::new(0, 1, 0))
    }

    fn status(registry: &PluginRegistry, id: &str) -> PluginStatus {
        registry
            .get(&PluginId::new(id).unwrap())
            .unwrap()
            .status
            .clone()
    }

    fn reasons(registry: &PluginRegistry, id: &str) -> String {
        match status(registry, id) {
            PluginStatus::Incompatible(reasons) => reasons.join("; "),
            other => panic!("{id} is {other:?}"),
        }
    }

    #[test]
    fn host_protocol_version_parses() {
        assert_eq!(
            protocol_version().to_string(),
            kumiko_protocol::PROTOCOL_VERSION
        );
    }

    #[test]
    fn api_mismatch_and_disabled_plugins() {
        let mut off = plugin("test.off", "1.0.0", "");
        off.enabled = false;
        let mut future = plugin("test.future", "1.0.0", "");
        future.manifest.plugin.api = "^1.0".parse().unwrap();
        let registry = resolve(vec![plugin("test.ok", "1.0.0", ""), off, future]);
        assert_eq!(status(&registry, "test.ok"), PluginStatus::Ready);
        assert_eq!(status(&registry, "test.off"), PluginStatus::Disabled);
        assert!(reasons(&registry, "test.future").contains("requires KPP ^1.0"));
        assert_eq!(registry.ready().count(), 1);
    }

    #[test]
    fn dependencies_cascade() {
        let registry = resolve(vec![
            plugin("test.base", "1.2.0", ""),
            plugin(
                "test.mid",
                "1.0.0",
                "[dependencies]\n\"test.base\" = \"^1.1\"",
            ),
            plugin("test.top", "1.0.0", "[dependencies]\n\"test.mid\" = \"^1\""),
            plugin(
                "test.old",
                "1.0.0",
                "[dependencies]\n\"test.base\" = \"^2\"",
            ),
            plugin(
                "test.lost",
                "1.0.0",
                "[dependencies]\n\"test.none\" = \"^1\"",
            ),
            plugin(
                "test.chain",
                "1.0.0",
                "[dependencies]\n\"test.lost\" = \"^1\"",
            ),
        ]);
        for ready in ["test.base", "test.mid", "test.top"] {
            assert_eq!(status(&registry, ready), PluginStatus::Ready, "{ready}");
        }
        assert!(reasons(&registry, "test.old").contains("needs plugin test.base ^2"));
        assert!(reasons(&registry, "test.lost").contains("test.none"));
        assert!(reasons(&registry, "test.chain").contains("needs plugin test.lost"));
    }

    #[test]
    fn required_and_optional_interfaces() {
        let provider = "[[provides]]\ninterface = \"test.git.repository\"\nversion = \"1.3.0\"";
        let consumer = |range: &str, optional: bool| {
            format!(
                "[[consumes]]\ninterface = \"test.git.repository\"\nversion = \"{range}\"\noptional = {optional}"
            )
        };
        let mut disabled_provider = plugin("test.git2", "1.0.0", provider);
        disabled_provider.enabled = false;
        let registry = resolve(vec![
            plugin("test.git", "1.0.0", provider),
            disabled_provider,
            plugin("test.ai", "1.0.0", &consumer("^1", false)),
            plugin("test.next", "1.0.0", &consumer("^2", false)),
            plugin("test.maybe", "1.0.0", &consumer("^2", true)),
        ]);
        assert_eq!(status(&registry, "test.ai"), PluginStatus::Ready);
        assert!(reasons(&registry, "test.next").contains("needs interface test.git.repository ^2"));
        assert_eq!(status(&registry, "test.maybe"), PluginStatus::Ready);
        let interface = InterfaceId::new("test.git.repository").unwrap();
        assert_eq!(
            registry.providers(&interface),
            [PluginId::new("test.git").unwrap()]
        );
    }

    #[test]
    fn commands_route_to_their_plugin() {
        let registry = resolve(vec![
            plugin(
                "test.echo",
                "1.0.0",
                "[[contributes.commands]]\nid = \"echo.say\"\ntitle = \"Say\"\n[[contributes.keybindings]]\nkey = \"ctrl+e\"\ncommand = \"echo.say\"",
            ),
            plugin(
                "test2.echo",
                "1.0.0",
                "[[contributes.commands]]\nid = \"echo.say\"\ntitle = \"Also say\"",
            ),
        ]);
        assert_eq!(
            registry.plugin_for_command("echo.say").unwrap().as_str(),
            "test.echo"
        );
        assert!(registry.plugin_for_command("echo.unknown").is_none());
        let commands: Vec<_> = registry
            .commands()
            .map(|(id, c)| (id.as_str(), c.title.as_str()))
            .collect();
        assert_eq!(commands, [("test.echo", "Say")]);
        assert_eq!(registry.problems().len(), 1);
        assert!(registry.problems()[0].contains("already contributed by test.echo"));
        assert_eq!(registry.keybindings().count(), 1);
    }

    #[test]
    fn diff_between_registries() {
        let ids = |list: &[PluginId]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let old = resolve(vec![
            plugin("test.keep", "1.0.0", ""),
            plugin("test.gone", "1.0.0", ""),
            plugin("test.upgrade", "1.0.0", ""),
        ]);
        let mut off = plugin("test.keep2", "1.0.0", "");
        off.enabled = false;
        let new = resolve(vec![
            plugin("test.keep", "1.0.0", ""),
            plugin("test.upgrade", "2.0.0", ""),
            plugin("test.new", "1.0.0", ""),
            off,
        ]);
        let diff = old.diff(&new);
        assert_eq!(ids(&diff.removed), ["test.gone", "test.upgrade"]);
        assert_eq!(ids(&diff.added), ["test.new", "test.upgrade"]);
        assert_eq!(PluginRegistry::default().diff(&old).added.len(), 3);
        assert_eq!(old.diff(&PluginRegistry::default()).removed.len(), 3);
    }

    #[test]
    fn activation_patterns_route_commands_too() {
        let mut process = plugin("test.java", "1.0.0", "");
        process.manifest = Manifest::parse(
            "[plugin]\nid = \"test.java\"\nname = \"Java\"\nversion = \"1.0.0\"\napi = \"^0.1\"\nkind = \"process\"\nentry = \"bin/java\"\nactivation = [\"onCommand:java.*\"]",
        )
        .unwrap();
        let registry = resolve(vec![process]);
        assert_eq!(
            registry.plugin_for_command("java.build").unwrap().as_str(),
            "test.java"
        );
        assert!(registry.plugin_for_command("javascript.build").is_none());
    }
}
