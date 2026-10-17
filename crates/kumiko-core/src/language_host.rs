//! Wiring of the Language service: servers come from the plugins' manifests, documents from
//! the Documents service's events, edits go back through the Documents service.

use std::sync::{Arc, Weak};

use kumiko_events::{Delivery, EventFilter};
use kumiko_lsp::{LanguageHost, LanguageService, ServerDecl, WorkspaceEdit};
use kumiko_protocol::events::{
    DiagnosticsChanged, DocumentChanged, DocumentClosed, DocumentOpened, DocumentSaved,
    LanguageStatusChanged,
};
use kumiko_protocol::methods::lsp::{
    LaunchSpec, LspMessageParams, MiddlewareResult, ResolveLaunchParams,
};
use kumiko_service_api::{BoxFuture, Event, EventSink, FileSystem};
use kumiko_types::{PluginId, Uri};

use crate::{CoreApi, Inner};

/// The output channel of language servers' logs.
pub const LANGUAGE_CHANNEL: &str = "kumiko.language";

/// The core's side of [`LanguageHost`]; holds the core weakly so servers do not keep it alive.
pub(crate) struct Host {
    pub(crate) core: Weak<Inner>,
}

impl Host {
    fn core(&self) -> Option<CoreApi> {
        self.core.upgrade().map(|inner| CoreApi { inner })
    }
}

impl LanguageHost for Host {
    fn resolve_launch(
        &self,
        server: &ServerDecl,
        language: &str,
        root: &Uri,
    ) -> BoxFuture<'static, Result<LaunchSpec, String>> {
        let core = self.core();
        let params = ResolveLaunchParams {
            server_id: server.id.clone(),
            language_id: language.to_owned(),
            workspace_folder: root.clone(),
        };
        let plugin = PluginId::new(server.plugin.clone());
        Box::pin(async move {
            let core = core.ok_or("the IDE is shutting down")?;
            let plugin = plugin.map_err(|e| e.to_string())?;
            core.plugin_host()
                .resolve_launch(&plugin, params)
                .await
                .map_err(|e| e.message)
        })
    }

    fn apply_edit(&self, edit: WorkspaceEdit) -> BoxFuture<'static, bool> {
        let core = self.core();
        Box::pin(async move {
            let Some(core) = core else {
                return false;
            };
            core.apply_workspace_edit(&edit).await.is_ok()
        })
    }

    fn read_text(&self, uri: &Uri) -> BoxFuture<'static, Option<String>> {
        let core = self.core();
        let uri = uri.clone();
        Box::pin(async move {
            let bytes = core?.inner.fs.read(&uri).await.ok()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        })
    }

    fn status_changed(&self) {
        if let Some(core) = self.core() {
            core.inner
                .events
                .publish(Event::core(&LanguageStatusChanged {}));
        }
    }

    fn diagnostics_changed(&self, uris: Vec<Uri>) {
        if let Some(core) = self.core() {
            core.inner
                .events
                .publish(Event::core(&DiagnosticsChanged { uris }));
        }
    }

    fn log(&self, server: &str, message: &str) {
        tracing::debug!(server, "{message}");
        if let Some(core) = self.core() {
            core.inner
                .options
                .output
                .append(LANGUAGE_CHANNEL, &format!("[{server}] {message}"));
        }
    }

    fn middleware(&self, server: &ServerDecl) -> Vec<String> {
        let (Some(core), Ok(plugin)) = (self.core(), PluginId::new(server.plugin.clone())) else {
            return Vec::new();
        };
        core.plugin_host().lsp_middleware(&plugin)
    }

    fn intercept(
        &self,
        server: &ServerDecl,
        method: &str,
        params: serde_json::Value,
    ) -> BoxFuture<'static, Result<MiddlewareResult, String>> {
        let core = self.core();
        let plugin = PluginId::new(server.plugin.clone());
        let message = LspMessageParams {
            server_id: server.id.clone(),
            method: method.to_owned(),
            params,
        };
        Box::pin(async move {
            let core = core.ok_or("the IDE is shutting down")?;
            let plugin = plugin.map_err(|e| e.to_string())?;
            core.plugin_host()
                .lsp_middleware_request(&plugin, message)
                .await
                .map_err(|e| e.message)
        })
    }

    fn document_version(&self, uri: &Uri) -> Option<u64> {
        let core = self.core()?;
        let id = core.inner.documents.find(uri)?;
        core.inner.documents.snapshot(id).ok().map(|s| s.version.0)
    }

    fn workspace_folders(&self) -> Vec<Uri> {
        self.core()
            .map(|core| {
                core.inner
                    .project
                    .folders()
                    .into_iter()
                    .map(|f| f.uri)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Language servers contributed by ready plugins.
pub(crate) fn server_decls(core: &CoreApi) -> Vec<ServerDecl> {
    core.plugins()
        .entries()
        .filter(|e| e.is_ready())
        .flat_map(|entry| {
            let plugin = entry.manifest().id().to_string();
            entry
                .manifest()
                .contributes
                .language_servers
                .iter()
                .map(move |s| ServerDecl {
                    id: s.id.clone(),
                    plugin: plugin.clone(),
                    languages: s.languages.clone(),
                })
        })
        .collect()
}

/// Feeds the Language service: documents as they open, change, save and close; servers as
/// plugins come and go.
pub(crate) fn spawn_wiring(core: &CoreApi) -> tokio::task::JoinHandle<()> {
    let topics = [
        "document.opened",
        "document.changed",
        "document.saved",
        "document.closed",
        "plugin.stateChanged",
    ];
    let mut events = core.inner.events.subscribe(EventFilter::topics(topics));
    let weak = Arc::downgrade(&core.inner);
    let language: LanguageService = core.inner.language.clone();
    language.set_servers(server_decls(core));
    tokio::spawn(async move {
        while let Some(delivery) = events.recv().await {
            let Some(inner) = weak.upgrade() else {
                break;
            };
            let core = CoreApi { inner };
            let event = match delivery {
                Delivery::Event(event) => event,
                Delivery::Resync(_) => {
                    // Lost events: start over from the open documents.
                    events = core.inner.events.subscribe(EventFilter::topics(topics));
                    for snapshot in core.inner.documents.list() {
                        language.did_close(&snapshot.uri);
                        if let Some(lang) = core.language_of(snapshot.id) {
                            language.did_open(
                                &snapshot.uri,
                                &lang,
                                snapshot.version.0,
                                snapshot.text,
                            );
                        }
                    }
                    continue;
                }
            };
            if event.topic == "plugin.stateChanged" {
                language.set_servers(server_decls(&core));
            } else if let Some(Ok(opened)) = event.decode::<DocumentOpened>() {
                let Some(id) = core.inner.documents.find(&opened.uri) else {
                    continue;
                };
                let (Ok(snapshot), Some(lang)) =
                    (core.inner.documents.snapshot(id), core.language_of(id))
                else {
                    continue;
                };
                language.did_open(&opened.uri, &lang, snapshot.version.0, snapshot.text);
            } else if let Some(Ok(changed)) = event.decode::<DocumentChanged>() {
                language.did_change(&changed.uri, changed.version.0, &changed.changes);
            } else if let Some(Ok(saved)) = event.decode::<DocumentSaved>() {
                language.did_save(&saved.uri);
            } else if let Some(Ok(closed)) = event.decode::<DocumentClosed>() {
                language.did_close(&closed.uri);
            }
        }
    })
}
