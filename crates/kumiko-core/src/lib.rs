//! CoreApi facade and composition of core services (spec, sections 8 and 11).
//!
//! `kumiko-core` is the only crate that knows every service: it creates them, hands them
//! their neighbours as traits (ADR-019) and wires events between them. UI crates and the
//! headless driver use the core only through [`CoreApi`] and the service types re-exported
//! here.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::task::JoinHandle;

pub use kumiko_commands as commands;
pub use kumiko_config as config;
pub use kumiko_documents as documents;
pub use kumiko_events as events;
pub use kumiko_fs as fs;
pub use kumiko_lsp as language;
pub use kumiko_plugin_host as plugin_host;
pub use kumiko_project as project;
pub use kumiko_protocol as protocol;
pub use kumiko_resources as resources;
pub use kumiko_search as search;
pub use kumiko_service_api as service_api;
pub use kumiko_syntax as syntax;
pub use kumiko_wasm_host as wasm_host;

use kumiko_commands::CommandRegistry;
use kumiko_config::keymap::{Keybinding, parse_keymap};
use kumiko_config::{ConfigService, KeymapSource};
use kumiko_documents::{DocumentService, DocumentsConfig};
use kumiko_events::{Delivery, EventBus, EventFilter};
use kumiko_fs::{FsWatcher, LocalFileSystem, WatchError, WatcherConfig};
use kumiko_plugin_host::{PluginHost, PluginRegistry, StoreError, SupervisorConfig};
use kumiko_project::{ProjectError, ProjectService};
use kumiko_protocol::common::WorkspaceFolder;
use kumiko_protocol::events::{ConfigChanged, FsChangeKind, FsChanged};
use kumiko_resources::{ResourceKind, ResourceRegistry};
use kumiko_search::{FileIndex, IndexRoot};
use kumiko_service_api::EventSink;
use kumiko_service_api::wasm::WasmRuntime;
use kumiko_syntax::{GrammarPackage, Highlight, SyntaxService};
use kumiko_types::DocumentId;
use kumiko_types::paths::workspace_settings_dir;
use kumiko_types::{KumikoDirs, PluginId};
use kumiko_wasm_host::{WasmConfig, WasmEngine};

mod language_host;
pub mod output;
mod plugins;
pub mod tasks;

pub use language_host::LANGUAGE_CHANNEL;

/// Settings the core itself reacts to.
const WATCHER_EXCLUDE: &str = "files.watcherExclude";
const FILES_EXCLUDE: &str = "files.exclude";
const IGNORE_FILES: &str = "search.ignoreFiles";
/// Quiet time before the file index is rebuilt after changes.
const REINDEX_DELAY: Duration = Duration::from_millis(300);

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error(transparent)]
    Watch(#[from] WatchError),
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Plugin(#[from] StoreError),
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
    /// Timeouts and restart policy of process plugins.
    pub plugins: SupervisorConfig,
    /// WASM runtime limits; the compilation cache defaults to `dirs.wasm_cache_dir()`.
    pub wasm: WasmConfig,
    /// Output channels; pass one in to have logging reach it from the start.
    pub output: output::Output,
}

impl CoreOptions {
    pub fn new(dirs: KumikoDirs) -> Self {
        let wasm = WasmConfig {
            cache_dir: Some(dirs.wasm_cache_dir()),
            ..WasmConfig::default()
        };
        Self {
            dirs,
            builtin_resources: None,
            locale: None,
            documents: DocumentsConfig::default(),
            watcher: WatcherConfig::default(),
            plugins: SupervisorConfig::default(),
            wasm,
            output: output::Output::new(),
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
    files: Arc<FileIndex>,
    syntax: Arc<SyntaxService>,
    language: kumiko_lsp::LanguageService,
    processes: Arc<dyn kumiko_service_api::Processes>,
    runs: tasks::Runs,
    reindex: Arc<tokio::sync::Notify>,
    plugins: plugins::PluginManager,
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

        // Grammars are L3 packages too (spec, section 18).
        let syntax = Arc::new(SyntaxService::new());
        let grammars = resources.list(ResourceKind::Grammar);
        let packages: Vec<GrammarPackage<'_>> = grammars
            .iter()
            .filter_map(|p| {
                Some(GrammarPackage {
                    dir: &p.dir,
                    grammar: p.manifest.get("grammar")?,
                })
            })
            .collect();
        for problem in syntax.load(&packages) {
            tracing::warn!("grammar: {problem}");
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
        // Without an engine the IDE still works; WASM plugins then fail to start with a reason.
        let wasm: Option<Arc<dyn WasmRuntime>> = match WasmEngine::new(options.wasm.clone()) {
            Ok(engine) => Some(Arc::new(engine)),
            Err(error) => {
                tracing::error!("{error}");
                None
            }
        };
        let language_slot = Arc::new(std::sync::OnceLock::new());
        let services = plugins::Services {
            commands: commands.clone(),
            config: config.clone(),
            project: project.clone(),
            events: sink.clone(),
            locale: options.locale.clone(),
            language: language_slot.clone(),
        };
        let plugins =
            plugins::PluginManager::start(&options.dirs, services, options.plugins.clone(), wasm);

        let processes: Arc<dyn kumiko_service_api::Processes> =
            Arc::new(kumiko_process::ProcessService::new());
        let core = Self {
            inner: Arc::new_cyclic(|weak| Inner {
                options,
                events,
                commands,
                config,
                fs,
                watcher,
                documents,
                project,
                resources,
                files: Arc::new(FileIndex::new()),
                syntax,
                language: kumiko_lsp::LanguageService::new(
                    processes.clone(),
                    Arc::new(language_host::Host { core: weak.clone() }),
                ),
                processes,
                runs: tasks::Runs::default(),
                reindex: Arc::new(tokio::sync::Notify::new()),
                plugins,
                tasks: Mutex::default(),
            }),
        };
        let _ = language_slot.set(core.inner.language.clone());
        core.spawn_wiring();
        core.spawn_indexer();
        let wiring = language_host::spawn_wiring(&core);
        core.inner
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(wiring);
        Ok(core)
    }

    /// Background reactions between services.
    fn spawn_wiring(&self) {
        let inner = &self.inner;
        let mut subscriber = inner.events.subscribe(EventFilter::topics([
            "fs.changed",
            "config.changed",
            "document.closed",
        ]));
        let (events, config, documents, watcher, project, reindex, syntax) = (
            inner.events.clone(),
            inner.config.clone(),
            inner.documents.clone(),
            inner.watcher.clone(),
            inner.project.clone(),
            inner.reindex.clone(),
            inner.syntax.clone(),
        );
        let user_keymap = inner.options.dirs.user_keymap_file();
        let task =
            tokio::spawn(async move {
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
                            subscriber = events.subscribe(EventFilter::topics([
                                "fs.changed",
                                "config.changed",
                                "document.closed",
                            ]));
                            continue;
                        }
                    };
                    if event.topic == "document.closed" {
                        // The closed document's tree goes with it.
                        let open: Vec<DocumentId> = documents.list().iter().map(|s| s.id).collect();
                        syntax.retain(&open);
                        continue;
                    }
                    if let Some(Ok(batch)) = event.decode::<FsChanged>() {
                        if batch.changes.iter().any(|c| {
                            matches!(c.kind, FsChangeKind::Created | FsChangeKind::Deleted)
                        }) {
                            reindex.notify_one();
                        }
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
                        if changed
                            .keys
                            .iter()
                            .any(|k| k == FILES_EXCLUDE || k == IGNORE_FILES)
                        {
                            reindex.notify_one();
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

    /// Rebuilds the file index a moment after it was asked to, once changes settle.
    fn spawn_indexer(&self) {
        // A weak handle: the task must not keep the core alive (its tasks stop on drop).
        let core = Arc::downgrade(&self.inner);
        let requests = self.inner.reindex.clone();
        let task = tokio::spawn(async move {
            loop {
                requests.notified().await;
                tokio::time::sleep(REINDEX_DELAY).await;
                let Some(inner) = core.upgrade() else { break };
                CoreApi { inner }.reindex_files().await;
            }
        });
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(task);
    }

    /// Rebuilds the file index of Quick Open now (on a blocking thread) and waits for it.
    pub async fn reindex_files(&self) {
        let roots: Vec<IndexRoot> = self
            .inner
            .project
            .folders()
            .into_iter()
            .filter_map(|f| {
                Some(IndexRoot {
                    path: f.uri.to_file_path()?,
                    name: f.name,
                })
            })
            .collect();
        let ignore_files = string_list(self.inner.config.get(IGNORE_FILES, None));
        let project = self.inner.project.clone();
        let files = self.inner.files.clone();
        let rebuilt = tokio::task::spawn_blocking(move || {
            let exclude = Arc::new(move |path: &Path| project.is_excluded(path));
            files.rebuild(&roots, &ignore_files, exclude);
        })
        .await;
        if let Err(error) = rebuilt {
            tracing::warn!("indexing files failed: {error}");
        }
    }

    /// The Syntax service (grammars, trees).
    pub fn syntax(&self) -> &SyntaxService {
        &self.inner.syntax
    }

    /// The Language service (language servers: diagnostics, completion, navigation, fixes).
    pub fn language(&self) -> &kumiko_lsp::LanguageService {
        &self.inner.language
    }

    /// The document's language: the one it was opened with, else by its file extension (a
    /// grammar package, then languages contributed by plugins).
    pub fn language_of(&self, document: DocumentId) -> Option<String> {
        let snapshot = self.inner.documents.snapshot(document).ok()?;
        snapshot.language_id.clone().or_else(|| {
            let path = snapshot.uri.to_file_path()?;
            self.inner.syntax.language_for_path(&path).or_else(|| {
                let extension = format!(".{}", path.extension()?.to_str()?);
                self.plugins()
                    .entries()
                    .filter(|e| e.is_ready())
                    .flat_map(|e| e.manifest().contributes.languages.iter())
                    .find(|l| {
                        l.extensions
                            .iter()
                            .any(|x| x.eq_ignore_ascii_case(&extension))
                    })
                    .map(|l| l.id.clone())
            })
        })
    }

    /// Applies edits across files (code actions, refactorings): an open document gets the
    /// edit as an unsaved change, a file that is not open is changed and saved.
    pub async fn apply_workspace_edit(
        &self,
        edit: &kumiko_lsp::WorkspaceEdit,
    ) -> Result<(), String> {
        let documents = &self.inner.documents;
        let mut failed = Vec::new();
        for (uri, edits) in &edit.changes {
            if edits.is_empty() {
                continue;
            }
            let was_open = documents.find(uri).is_some();
            let snapshot = match documents.open(uri, None).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    failed.push(format!("{uri}: {error}"));
                    continue;
                }
            };
            if let Err(error) = documents.apply(snapshot.id, snapshot.version, edits.clone()) {
                failed.push(format!("{uri}: {error}"));
            } else if !was_open && let Err(error) = documents.save(snapshot.id).await {
                failed.push(format!("{uri}: {error}"));
            }
            if !was_open {
                let _ = documents.close(snapshot.id);
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    /// Brings the document's syntax tree to its current version (incrementally when the
    /// edits since are still known). Large files get no tree (spec, section 12).
    fn sync_syntax(&self, document: DocumentId) -> bool {
        let Ok(snapshot) = self.inner.documents.snapshot(document) else {
            return false;
        };
        if snapshot.large {
            return false;
        }
        let Some(language) = self.language_of(document) else {
            return false;
        };
        let documents = &self.inner.documents;
        self.inner.syntax.sync(
            document,
            &language,
            snapshot.version,
            &snapshot.text,
            |version| documents.changes_since(document, version),
        )
    }

    /// Highlights of a byte range of the document, at its current version.
    pub fn highlights(
        &self,
        document: DocumentId,
        range: std::ops::Range<usize>,
    ) -> Vec<Highlight> {
        if self.sync_syntax(document) {
            self.inner.syntax.highlights(document, range)
        } else {
            Vec::new()
        }
    }

    /// Foldable regions of the document as `(first line, last line)`.
    pub fn folds(&self, document: DocumentId) -> Vec<(usize, usize)> {
        if self.sync_syntax(document) {
            self.inner.syntax.folds(document)
        } else {
            Vec::new()
        }
    }

    /// Output channels (the Output panel).
    pub fn output(&self) -> &output::Output {
        &self.inner.options.output
    }

    /// Files of the workspace for Quick Open.
    pub fn file_index(&self) -> &FileIndex {
        &self.inner.files
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
        self.inner.reindex.notify_one();
        Ok(folder)
    }

    pub fn dirs(&self) -> &KumikoDirs {
        &self.inner.options.dirs
    }

    /// Effective UI locale, e.g. `ru` or `en-US`.
    pub fn locale(&self) -> String {
        effective_locale(self.inner.options.locale.as_deref(), &self.inner.config)
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

    /// Installed plugins with their status and contributions, as of now.
    pub fn plugins(&self) -> Arc<PluginRegistry> {
        self.inner.plugins.registry()
    }

    /// Running process plugins: states, starts and stops.
    pub fn plugin_host(&self) -> &PluginHost {
        self.inner.plugins.host()
    }

    /// Installs (or upgrades) the plugin in the directory `source`; usable immediately.
    pub async fn install_plugin(&self, source: &Path) -> Result<PluginId, CoreError> {
        Ok(self.inner.plugins.install(source).await?)
    }

    /// Stops and deletes the plugin; its commands, keybindings and settings disappear.
    pub async fn uninstall_plugin(&self, plugin: &PluginId) -> Result<(), CoreError> {
        Ok(self.inner.plugins.uninstall(plugin).await?)
    }

    /// Enables or disables the plugin; a disabled plugin keeps its files but contributes nothing.
    pub async fn set_plugin_enabled(
        &self,
        plugin: &PluginId,
        enabled: bool,
    ) -> Result<(), CoreError> {
        Ok(self.inner.plugins.set_enabled(plugin, enabled).await?)
    }

    /// Stops plugins gracefully; call before dropping the last handle when the IDE exits.
    pub async fn shutdown(&self) {
        self.inner.language.shutdown().await;
        self.inner.plugins.host().stop_all().await;
    }
}

/// The forced locale, else `workbench.locale`, else `LANG`, else `en`.
pub(crate) fn effective_locale(forced: Option<&str>, config: &ConfigService) -> String {
    if let Some(locale) = forced {
        return locale.to_owned();
    }
    if let Some(Value::String(locale)) = config.get("workbench.locale", None) {
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
