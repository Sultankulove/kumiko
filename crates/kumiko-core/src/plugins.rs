//! Plugins in the core: resolve installed plugins at startup, register what the ready ones
//! contribute before any plugin code runs (spec, section 13), and serve the Plugin Host.

use std::sync::{Arc, Weak};

use serde_json::{Value, json};

use kumiko_commands::{CommandError, CommandInfo, CommandRegistry, Owner};
use kumiko_config::keymap::{KeySequence, Keybinding};
use kumiko_config::{ConfigService, KeymapSource};
use kumiko_plugin_host::{
    HostApi, Manifest, PluginEntry, PluginHost, PluginRegistry, PluginStatus, PluginStore, Session,
    SupervisorConfig, protocol_version,
};
use kumiko_project::ProjectService;
use kumiko_protocol::common::CommandCall;
use kumiko_protocol::events::PluginState;
use kumiko_protocol::methods::lifecycle::WorkspaceInfo;
use kumiko_protocol::{ErrorCode, ResponseError};
use kumiko_service_api::when::WhenExpr;
use kumiko_service_api::{BoxFuture, EventSink};
use kumiko_types::{KumikoDirs, PluginId};

pub(crate) struct Services {
    pub commands: Arc<CommandRegistry>,
    pub config: Arc<ConfigService>,
    pub project: Arc<ProjectService>,
    pub events: Arc<dyn EventSink>,
    /// Locale forced by the options; otherwise it follows the settings.
    pub locale: Option<String>,
}

pub(crate) struct Plugins {
    pub registry: Arc<PluginRegistry>,
    pub host: PluginHost,
}

pub(crate) fn load(
    dirs: &KumikoDirs,
    services: &Services,
    supervisor: SupervisorConfig,
) -> Plugins {
    let registry = resolve(dirs);
    let api = Arc::new(CoreHostApi {
        // Weak: command handlers own the host, the host must not own the commands.
        commands: Arc::downgrade(&services.commands),
        config: services.config.clone(),
        project: services.project.clone(),
        locale: services.locale.clone(),
    });
    let host = PluginHost::new(&registry, api, services.events.clone(), supervisor);

    register_commands(&registry, &services.commands, &host);
    set_context_keys(&registry, &services.commands);
    services
        .config
        .set_keymap_source(KeymapSource::Plugin, keybindings(&registry));
    for entry in registry.ready() {
        register_settings(entry, &services.config);
    }
    Plugins {
        registry: Arc::new(registry),
        host,
    }
}

fn resolve(dirs: &KumikoDirs) -> PluginRegistry {
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
    registry
}

fn register_commands(registry: &PluginRegistry, commands: &CommandRegistry, host: &PluginHost) {
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
        // The first call starts the plugin; the host waits for its handshake.
        let (owner, command_id, host) = (plugin.clone(), command.id.clone(), host.clone());
        let handler = Arc::new(move |args: Vec<Value>| -> kumiko_commands::CommandFuture {
            let (owner, host) = (owner.clone(), host.clone());
            let call = CommandCall {
                command: command_id.clone(),
                args,
            };
            Box::pin(async move {
                host.invoke_command(&owner, call)
                    .await
                    .map_err(|error| CommandError::Failed(error.message))
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

/// The core as the Plugin Host sees it.
struct CoreHostApi {
    commands: Weak<CommandRegistry>,
    config: Arc<ConfigService>,
    project: Arc<ProjectService>,
    locale: Option<String>,
}

impl HostApi for CoreHostApi {
    fn execute_command(
        &self,
        _caller: &PluginId,
        call: CommandCall,
    ) -> BoxFuture<'static, Result<Value, ResponseError>> {
        let commands = self.commands.upgrade();
        Box::pin(async move {
            let Some(commands) = commands else {
                return Err(ResponseError::new(
                    ErrorCode::InternalError,
                    "the core is stopping",
                ));
            };
            commands
                .execute(&call.command, call.args)
                .await
                .map_err(|error| {
                    let code = match &error {
                        CommandError::NotFound(_) => ErrorCode::NotFound,
                        CommandError::Disabled(_) | CommandError::AlreadyRegistered(_) => {
                            ErrorCode::InvalidRequest
                        }
                        CommandError::Failed(_) => ErrorCode::InternalError,
                    };
                    ResponseError::new(code, error.to_string())
                })
        })
    }

    fn set_context(&self, key: String, value: Value) {
        if let Some(commands) = self.commands.upgrade() {
            commands.context().set(key, value);
        }
    }

    fn session(&self, plugin: &Manifest) -> Session {
        Session {
            workspace: WorkspaceInfo {
                folders: self.project.folders(),
            },
            config: Value::Object(self.config.section(plugin.namespace(), None)),
            locale: crate::effective_locale(self.locale.as_deref(), &self.config),
        }
    }

    fn state_changed(&self, plugin: &Manifest, state: PluginState) {
        let Some(commands) = self.commands.upgrade() else {
            return;
        };
        let context = commands.context();
        let running = state == PluginState::Running;
        context.set(format!("plugin.{}.running", plugin.id()), json!(running));
        if !running {
            // Context keys the plugin set at runtime die with its process.
            context.remove_prefix(&format!("{}.", plugin.namespace()));
        }
    }
}
