//! Plugins at startup: resolve installed plugins and register what the ready ones contribute
//! (spec, section 13: contributions are registered before any plugin code runs).

use std::sync::Arc;

use serde_json::{Value, json};

use kumiko_commands::{CommandError, CommandInfo, CommandRegistry, Owner};
use kumiko_config::keymap::{KeySequence, Keybinding};
use kumiko_config::{ConfigService, KeymapSource};
use kumiko_plugin_host::{
    PluginEntry, PluginRegistry, PluginStatus, PluginStore, protocol_version,
};
use kumiko_service_api::when::WhenExpr;
use kumiko_types::{KumikoDirs, PluginId};

pub(crate) fn load(
    dirs: &KumikoDirs,
    commands: &CommandRegistry,
    config: &ConfigService,
) -> PluginRegistry {
    let scan = PluginStore::new(dirs).scan();
    for problem in &scan.problems {
        tracing::warn!("plugin store: {problem}");
    }
    let registry = PluginRegistry::resolve(scan.plugins, &protocol_version());
    for entry in registry.entries() {
        if let PluginStatus::Incompatible(reasons) = &entry.status {
            tracing::warn!(plugin = %entry.manifest().id(), "not loaded: {}", reasons.join("; "));
        }
    }
    for problem in registry.problems() {
        tracing::warn!("plugins: {problem}");
    }

    register_commands(&registry, commands);
    set_context_keys(&registry, commands);
    config.set_keymap_source(KeymapSource::Plugin, keybindings(&registry));
    for entry in registry.ready() {
        register_settings(entry, config);
    }
    registry
}

fn register_commands(registry: &PluginRegistry, commands: &CommandRegistry) {
    for (plugin, command) in registry.commands() {
        let info = CommandInfo {
            id: command.id.clone(),
            title: Some(command.title.clone()),
            owner: Owner::Plugin(plugin.clone()),
            when: command
                .when
                .as_deref()
                .and_then(|w| WhenExpr::parse(w).ok()),
        };
        // Until the plugin bridge exists, the command is visible but cannot run.
        let owner = plugin.clone();
        let handler = Arc::new(move |_args: Vec<Value>| -> kumiko_commands::CommandFuture {
            let owner = owner.clone();
            Box::pin(async move {
                Err(CommandError::Failed(format!(
                    "plugin {owner} is not running"
                )))
            })
        });
        if let Err(error) = commands.register(info, handler) {
            tracing::warn!(%plugin, "{error}");
        }
    }
}

/// `plugin.<id>.running` for ready plugins and `interface.<id>.available` for every interface
/// a plugin provides or consumes.
fn set_context_keys(registry: &PluginRegistry, commands: &CommandRegistry) {
    let context = commands.context();
    for entry in registry.ready() {
        context.set(
            format!("plugin.{}.running", entry.manifest().id()),
            json!(false),
        );
    }
    for entry in registry.entries() {
        let manifest = entry.manifest();
        let interfaces = manifest
            .provides
            .iter()
            .map(|p| &p.interface)
            .chain(manifest.consumes.iter().map(|c| &c.interface));
        for interface in interfaces {
            let available = !registry.providers(interface).is_empty();
            context.set(format!("interface.{interface}.available"), json!(available));
        }
    }
}

fn keybindings(registry: &PluginRegistry) -> Vec<Keybinding> {
    let mut bindings = Vec::new();
    for (plugin, binding) in registry.keybindings() {
        match KeySequence::parse(&binding.key) {
            Ok(key) => bindings.push(Keybinding {
                key,
                command: binding.command.clone(),
                when: binding
                    .when
                    .as_deref()
                    .and_then(|w| WhenExpr::parse(w).ok()),
                source: KeymapSource::Plugin,
            }),
            Err(error) => tracing::warn!(%plugin, key = %binding.key, "{error}"),
        }
    }
    bindings
}

/// Registers the plugin's settings schema; every key must lie in the plugin's namespace.
fn register_settings(entry: &PluginEntry, config: &ConfigService) {
    let manifest = entry.manifest();
    let Some(configuration) = &manifest.contributes.configuration else {
        return;
    };
    let plugin: &PluginId = manifest.id();
    let path = configuration.schema.resolve(&entry.plugin.dir);
    let schema: Value = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(schema) => schema,
        Err(error) => {
            tracing::warn!(%plugin, path = %path.display(), "settings schema: {error}");
            return;
        }
    };
    let foreign: Vec<&String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .keys()
                .filter(|k| !manifest.owns_key(k))
                .collect()
        })
        .unwrap_or_default();
    if !foreign.is_empty() {
        tracing::warn!(
            %plugin,
            ?foreign,
            "settings schema refused: keys outside the plugin namespace `{}.*`",
            manifest.namespace()
        );
        return;
    }
    if let Err(error) = config.register_schema(plugin.as_str(), &schema) {
        tracing::warn!(%plugin, path = %path.display(), "settings schema: {error}");
    }
}
