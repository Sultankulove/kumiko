//! Plugins in the core: resolve installed plugins, register what the ready ones contribute
//! before any plugin code runs (spec, section 13), apply installs, removals and enable flags at
//! runtime, and serve the Plugin Host.

use std::path::Path;
use std::sync::{Arc, Weak};

use serde_json::{Value, json};

use kumiko_commands::{CommandError, CommandInfo, CommandRegistry, Owner};
use kumiko_config::keymap::{KeySequence, Keybinding};
use kumiko_config::{ConfigService, KeymapSource};
use kumiko_plugin_host::{
    HostApi, Manifest, PluginEntry, PluginHost, PluginRegistry, PluginStatus, PluginStore,
    RegistryDiff, Session, StoreError, SupervisorConfig, protocol_version,
};
use kumiko_project::ProjectService;
use kumiko_protocol::common::CommandCall;
use kumiko_protocol::events::{InterfaceAvailabilityChanged, PluginState, PluginStateChanged};
use kumiko_protocol::methods::lifecycle::WorkspaceInfo;
use kumiko_protocol::{ErrorCode, ResponseError};
use kumiko_service_api::wasm::WasmRuntime;
use kumiko_service_api::when::WhenExpr;
use kumiko_service_api::{BoxFuture, Event, EventSink};
use kumiko_types::{InterfaceId, KumikoDirs, PluginId};

pub(crate) struct Services {
    pub commands: Arc<CommandRegistry>,
    pub config: Arc<ConfigService>,
    pub project: Arc<ProjectService>,
    pub events: Arc<dyn EventSink>,
    /// Locale forced by the options; otherwise it follows the settings.
    pub locale: Option<String>,
}

/// Installed plugins and everything the core registered for them.
pub(crate) struct PluginManager {
    store: PluginStore,
    services: Services,
    host: PluginHost,
    /// Serializes install, uninstall, enable and disable.
    operation: tokio::sync::Mutex<()>,
}

impl PluginManager {
    pub fn start(
        dirs: &KumikoDirs,
        services: Services,
        supervisor: SupervisorConfig,
        wasm: Option<Arc<dyn WasmRuntime>>,
    ) -> Self {
        let store = PluginStore::new(dirs);
        let registry = Arc::new(resolve(&store));
        let api = Arc::new(CoreHostApi {
            // Weak: command handlers own the host, the host must not own the commands.
            commands: Arc::downgrade(&services.commands),
            config: services.config.clone(),
            project: services.project.clone(),
            locale: services.locale.clone(),
        });
        let host = PluginHost::new(
            registry.clone(),
            api,
            services.events.clone(),
            wasm,
            supervisor,
        );
        let manager = Self {
            store,
            services,
            host,
            operation: tokio::sync::Mutex::new(()),
        };
        let empty = PluginRegistry::default();
        manager.apply(&empty, &registry, &empty.diff(&registry));
        manager
    }

    pub fn registry(&self) -> Arc<PluginRegistry> {
        self.host.registry()
    }

    pub fn host(&self) -> &PluginHost {
        &self.host
    }

    /// Installs (or upgrades) the plugin in `source` and makes it available right away.
    pub async fn install(&self, source: &Path) -> Result<PluginId, StoreError> {
        let _operation = self.operation.lock().await;
        let installed = self.store.install(source)?;
        self.reload().await;
        Ok(installed.manifest.id().clone())
    }

    /// Stops the plugin, removes its contributions and deletes it.
    pub async fn uninstall(&self, plugin: &PluginId) -> Result<(), StoreError> {
        let _operation = self.operation.lock().await;
        // Stop before deleting, so the process is not left running from a removed directory.
        self.host.stop(plugin).await;
        self.store.uninstall(plugin)?;
        self.reload().await;
        Ok(())
    }

    pub async fn set_enabled(&self, plugin: &PluginId, enabled: bool) -> Result<(), StoreError> {
        let _operation = self.operation.lock().await;
        self.store.set_enabled(plugin, enabled)?;
        self.reload().await;
        Ok(())
    }

    /// Re-reads the store and applies the difference: plugins that stopped being ready are
    /// stopped and unregistered, new ones registered. Dependents follow their dependencies.
    async fn reload(&self) {
        let old = self.host.registry();
        let new = Arc::new(resolve(&self.store));
        let diff = self.host.replace_registry(new.clone()).await;
        self.apply(&old, &new, &diff);
    }

    fn apply(&self, old: &PluginRegistry, new: &PluginRegistry, diff: &RegistryDiff) {
        let Services {
            commands,
            config,
            events,
            ..
        } = &self.services;
        let context = commands.context();
        for plugin in &diff.removed {
            commands.unregister_owner(&Owner::Plugin(plugin.clone()));
            config.unregister_schema(plugin.as_str());
            context.set(format!("plugin.{plugin}.running"), Value::Null);
            if let Some(entry) = old.get(plugin) {
                context.remove_prefix(&format!("{}.", entry.manifest().namespace()));
            }
        }
        register_commands(new, commands, &self.host, &diff.added);
        for plugin in &diff.added {
            if let Some(entry) = new.get(plugin) {
                register_settings(entry, config);
            }
            context.set(format!("plugin.{plugin}.running"), json!(false));
        }
        config.set_keymap_source(KeymapSource::Plugin, keybindings(new));
        update_interfaces(old, new, commands, events.as_ref());

        for plugin in diff.removed.iter().filter(|p| !diff.added.contains(p)) {
            let (state, reason) = match new.get(plugin).map(|e| &e.status) {
                Some(PluginStatus::Disabled) => (PluginState::Disabled, None),
                Some(PluginStatus::Incompatible(reasons)) => {
                    (PluginState::Installed, Some(reasons.join("; ")))
                }
                // Uninstalled: nothing is left to have a state.
                Some(PluginStatus::Ready) | None => continue,
            };
            let changed = PluginStateChanged {
                plugin: plugin.clone(),
                state,
                reason,
            };
            events.publish(Event::core(&changed));
        }
        for plugin in &diff.added {
            let changed = PluginStateChanged {
                plugin: plugin.clone(),
                state: PluginState::Enabled,
                reason: None,
            };
            events.publish(Event::core(&changed));
        }
    }
}

fn resolve(store: &PluginStore) -> PluginRegistry {
    let scan = store.scan();
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

/// Registers the commands of `plugins`; each call starts the plugin if needed.
fn register_commands(
    registry: &PluginRegistry,
    commands: &CommandRegistry,
    host: &PluginHost,
    plugins: &[PluginId],
) {
    for (plugin, command) in registry.commands().filter(|(p, _)| plugins.contains(p)) {
        let info = CommandInfo {
            id: command.id.clone(),
            title: Some(command.title.clone()),
            owner: Owner::Plugin(plugin.clone()),
            when: command
                .when
                .as_deref()
                .and_then(|w| WhenExpr::parse(w).ok()),
        };
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

/// `interface.<id>.available` for every interface a plugin provides or consumes, with
/// `interface.availabilityChanged` when it flips.
fn update_interfaces(
    old: &PluginRegistry,
    new: &PluginRegistry,
    commands: &CommandRegistry,
    events: &dyn EventSink,
) {
    let mut interfaces: Vec<&InterfaceId> = old
        .entries()
        .chain(new.entries())
        .flat_map(|entry| {
            let manifest = entry.manifest();
            manifest
                .provides
                .iter()
                .map(|p| &p.interface)
                .chain(manifest.consumes.iter().map(|c| &c.interface))
        })
        .collect();
    interfaces.sort();
    interfaces.dedup();
    for interface in interfaces {
        let before = !old.providers(interface).is_empty();
        let available = !new.providers(interface).is_empty();
        commands
            .context()
            .set(format!("interface.{interface}.available"), json!(available));
        if before != available {
            events.publish(Event::core(&InterfaceAvailabilityChanged {
                interface: interface.clone(),
                available,
            }));
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

    fn preferred_provider(&self, interface: &InterfaceId) -> Option<PluginId> {
        let key = format!("interfaces.{interface}.provider");
        self.config
            .get(&key, None)
            .and_then(|value| value.as_str().and_then(|id| PluginId::new(id).ok()))
    }
}
