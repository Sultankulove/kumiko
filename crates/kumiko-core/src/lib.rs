//! CoreApi facade and composition of core services (spec, sections 8 and 11).
//!
//! `kumiko-core` is the only crate that knows every service: it creates them, hands them
//! their neighbours as traits (ADR-019) and wires events between them. UI crates and the
//! headless driver use the core only through [`CoreApi`] and the service types re-exported
//! here.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::task::JoinHandle;

pub use kumiko_commands as commands;
pub use kumiko_config as config;
pub use kumiko_documents as documents;
pub use kumiko_events as events;
pub use kumiko_fs as fs;
pub use kumiko_plugin_host as plugin_host;
pub use kumiko_project as project;
pub use kumiko_protocol as protocol;
pub use kumiko_resources as resources;
pub use kumiko_service_api as service_api;

use kumiko_commands::CommandRegistry;
use kumiko_config::keymap::{Keybinding, parse_keymap};
use kumiko_config::{ConfigService, KeymapSource};
use kumiko_documents::{DocumentService, DocumentsConfig};
use kumiko_events::{Delivery, EventBus, EventFilter};
use kumiko_fs::{FsWatcher, LocalFileSystem, WatchError, WatcherConfig};
use kumiko_plugin_host::PluginRegistry;
use kumiko_project::{ProjectError, ProjectService};
use kumiko_protocol::common::WorkspaceFolder;
use kumiko_protocol::events::{ConfigChanged, FsChanged};
use kumiko_resources::{ResourceKind, ResourceRegistry};
use kumiko_service_api::EventSink;
use kumiko_types::KumikoDirs;
use kumiko_types::paths::workspace_settings_dir;

mod plugins;

/// Settings the core itself reacts to.
const WATCHER_EXCLUDE: &str = "files.watcherExclude";
const FILES_EXCLUDE: &str = "files.exclude";

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error(transparent)]
    Watch(#[from] WatchError),
    #[error(transparent)]
    Project(#[from] ProjectError),
}

#[derive(Debug, Clone)]
pub struct CoreOptions {
    pub dirs: KumikoDirs,
    /// Resources shipped with the binary; user resources come from `dirs`.
    pub builtin_resources: Option<PathBuf>,
    /// UI locale; `None` takes `workbench.locale`, then `LANG`, then `en`.
    pub locale: Option<String>,
    pub documents: DocumentsConfig,
    pub watcher: WatcherConfig,
}

impl CoreOptions {
    pub fn new(dirs: KumikoDirs) -> Self {
        Self {
            dirs,
            builtin_resources: None,
            locale: None,
            documents: DocumentsConfig::default(),
            watcher: WatcherConfig::default(),
        }
    }
}

struct Inner {
    options: CoreOptions,
    events: Arc<EventBus>,
    commands: Arc<CommandRegistry>,
    config: Arc<ConfigService>,
    fs: Arc<LocalFileSystem>,
    watcher: Arc<FsWatcher>,
    documents: Arc<DocumentService>,
    project: Arc<ProjectService>,
    resources: Arc<ResourceRegistry>,
    plugins: Arc<PluginRegistry>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for task in self
            .tasks
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
        {
            task.abort();
        }
    }
}

/// Handle to a running core; cheap to clone.
#[derive(Clone)]
pub struct CoreApi {
    inner: Arc<Inner>,
}

fn io_error(path: &Path, error: impl std::fmt::Display) -> CoreError {
    CoreError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn string_list(value: Option<Value>) -> Vec<String> {
    value
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

impl CoreApi {
    /// Creates the services and starts the core's background tasks on the current runtime.
    pub async fn start(options: CoreOptions) -> Result<Self, CoreError> {
        let events = Arc::new(EventBus::new());
        let sink: Arc<dyn EventSink> = events.clone();

        let resources = Arc::new(ResourceRegistry::new());
        let mut roots: Vec<PathBuf> = options.builtin_resources.iter().cloned().collect();
        roots.push(options.dirs.resources_dir());
        for problem in resources.scan(&roots) {
            tracing::warn!(path = %problem.path.display(), "{}", problem.message);
        }

        let config = Arc::new(ConfigService::new(sink.clone()));
        if let Some(builtin) = &options.builtin_resources {
            let path = builtin.join("settings/core.schema.json");
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let schema: Value =
                        serde_json::from_str(&text).map_err(|e| io_error(&path, e))?;
                    config
                        .register_schema("core", &schema)
                        .map_err(|e| io_error(&path, e))?;
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "no core settings schema")
                }
            }
        }
        let config_dir = options.dirs.config_dir().to_path_buf();
        std::fs::create_dir_all(&config_dir).map_err(|e| io_error(&config_dir, e))?;
        config.set_user_file(Some(options.dirs.user_settings_file()));
        config.set_keymap_source(KeymapSource::Default, default_keymap(&resources));
        config.load_keymap_file(KeymapSource::User, &options.dirs.user_keymap_file());

        let watcher_config = WatcherConfig {
            exclude: string_list(config.get(WATCHER_EXCLUDE, None)),
            ..options.watcher.clone()
        };
        let watcher = Arc::new(FsWatcher::start(sink.clone(), watcher_config)?);
        watcher.watch(&config_dir)?;

        let fs = Arc::new(LocalFileSystem::new());
        let documents = Arc::new(DocumentService::new(
            fs.clone(),
            sink.clone(),
            options.documents.clone(),
        ));
        let project = Arc::new(ProjectService::new(sink.clone()));
        project.set_exclude(&string_list(config.get(FILES_EXCLUDE, None)))?;
        let commands = Arc::new(CommandRegistry::new());
        commands.context().set("workspaceFolderCount", json!(0));
        let plugins = Arc::new(plugins::load(&options.dirs, &commands, &config));

        let core = Self {
            inner: Arc::new(Inner {
                options,
                events,
                commands,
                config,
                fs,
                watcher,
                documents,
                project,
                resources,
                plugins,
                tasks: Mutex::default(),
            }),
        };
        core.spawn_wiring();
        Ok(core)
    }

    /// Background reactions between services.
    fn spawn_wiring(&self) {
        let inner = &self.inner;
        let mut subscriber = inner
            .events
            .subscribe(EventFilter::topics(["fs.changed", "config.changed"]));
        let (events, config, documents, watcher, project) = (
            inner.events.clone(),
            inner.config.clone(),
            inner.documents.clone(),
            inner.watcher.clone(),
            inner.project.clone(),
        );
        let user_keymap = inner.options.dirs.user_keymap_file();
        let task = tokio::spawn(async move {
            while let Some(delivery) = subscriber.recv().await {
                let event = match delivery {
                    Delivery::Event(event) => event,
                    Delivery::Resync(topics) => {
                        // Lost state: re-read everything we derive from events.
                        tracing::warn!(?topics, "core wiring resyncs");
                        config.reload();
                        for doc in documents.list() {
                            let _ = documents.external_change(&doc.uri).await;
                        }
                        subscriber =
                            events.subscribe(EventFilter::topics(["fs.changed", "config.changed"]));
                        continue;
                    }
                };
                if let Some(Ok(batch)) = event.decode::<FsChanged>() {
                    let config_files = config.files();
                    let mut reload_config = false;
                    for change in batch.changes {
                        let Some(path) = change.uri.to_file_path() else {
                            continue;
                        };
                        // An ancestor counts too: a file created together with its new
                        // directory may be reported only as the directory's creation.
                        if config_files.iter().any(|file| file.starts_with(&path)) {
                            reload_config = true;
                        }
                        if path == user_keymap {
                            config.load_keymap_file(KeymapSource::User, &path);
                        }
                        if let Err(error) = documents.external_change(&change.uri).await {
                            tracing::warn!(uri = %change.uri, %error, "external change failed");
                        }
                    }
                    if reload_config {
                        config.reload();
                    }
                } else if let Some(Ok(changed)) = event.decode::<ConfigChanged>() {
                    if changed.keys.iter().any(|k| k == WATCHER_EXCLUDE)
                        && let Err(error) =
                            watcher.set_exclude(&string_list(config.get(WATCHER_EXCLUDE, None)))
                    {
                        tracing::warn!(%error, "invalid watcher excludes");
                    }
                    if changed.keys.iter().any(|k| k == FILES_EXCLUDE)
                        && let Err(error) =
                            project.set_exclude(&string_list(config.get(FILES_EXCLUDE, None)))
                    {
                        tracing::warn!(%error, "invalid file excludes");
                    }
                }
            }
        });
        inner
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(task);
    }

    /// Opens a folder: adds it to the workspace, watches it and loads its settings. In the
    /// MVP the first folder's `.kumiko/settings.toml` is the workspace settings layer.
    pub async fn open_folder(&self, path: &Path) -> Result<WorkspaceFolder, CoreError> {
        let first = self.inner.project.folders().is_empty();
        let folder = self.inner.project.add_folder(path)?;
        let root = folder
            .uri
            .to_file_path()
            .unwrap_or_else(|| path.to_path_buf());
        self.inner.watcher.watch(&root)?;
        if first {
            let settings = workspace_settings_dir(&root).join("settings.toml");
            self.inner.config.set_workspace_file(Some(settings));
        }
        self.inner.commands.context().set(
            "workspaceFolderCount",
            json!(self.inner.project.folders().len()),
        );
        Ok(folder)
    }

    pub fn dirs(&self) -> &KumikoDirs {
        &self.inner.options.dirs
    }

    /// Effective UI locale, e.g. `ru` or `en-US`.
    pub fn locale(&self) -> String {
        if let Some(locale) = &self.inner.options.locale {
            return locale.clone();
        }
        if let Some(Value::String(locale)) = self.inner.config.get("workbench.locale", None) {
            return locale;
        }
        std::env::var("LANG")
            .ok()
            .and_then(|lang| {
                let lang = lang.split('.').next()?.replace('_', "-");
                (!lang.is_empty() && lang != "C" && lang != "POSIX").then_some(lang)
            })
            .unwrap_or_else(|| "en".into())
    }

    pub fn events(&self) -> &EventBus {
        &self.inner.events
    }

    pub fn commands(&self) -> &CommandRegistry {
        &self.inner.commands
    }

    pub fn config(&self) -> &ConfigService {
        &self.inner.config
    }

    pub fn fs(&self) -> &LocalFileSystem {
        &self.inner.fs
    }

    pub fn documents(&self) -> &DocumentService {
        &self.inner.documents
    }

    pub fn project(&self) -> &ProjectService {
        &self.inner.project
    }

    pub fn resources(&self) -> &ResourceRegistry {
        &self.inner.resources
    }

    /// Installed plugins resolved at startup, with their status and contributions.
    pub fn plugins(&self) -> &PluginRegistry {
        &self.inner.plugins
    }
}

/// Bindings of every keymap resource package.
fn default_keymap(resources: &ResourceRegistry) -> Vec<Keybinding> {
    let mut bindings = Vec::new();
    for package in resources.list(ResourceKind::Keymap) {
        let path = package.manifest_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (parsed, problems) = parse_keymap(&text, KeymapSource::Default);
                for problem in problems {
                    tracing::warn!(path = %path.display(), "{problem}");
                }
                bindings.extend(parsed);
            }
            Err(error) => tracing::warn!(path = %path.display(), %error, "unreadable keymap"),
        }
    }
    bindings
}
