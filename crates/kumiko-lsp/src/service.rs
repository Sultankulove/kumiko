//! The Language service: which server serves a document, starting servers through the plugin
//! that contributes them, document sync, and the features the editor asks for.
//!
//! - One server instance per contributed server and workspace folder ([`ServerKey`]).
//! - A server starts when the first document of one of its languages opens; its command line
//!   comes from the plugin (`lsp/resolveLaunch`), the process from the Process service.
//! - The service keeps its own copy of every open document at the version the servers know:
//!   edits are converted to the server's encoding against it and sent after a 50 ms debounce,
//!   or right before a request about the document.
//! - A server that exits is restarted (up to [`MAX_RESTARTS`] times) and gets its documents
//!   again with `didOpen`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use ropey::Rope;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;

use kumiko_protocol::methods::lsp::{LaunchSpec, MiddlewareResult};
use kumiko_service_api::{BoxFuture, ProcessControl, ProcessSpec, Processes};
use kumiko_types::{Position, Range, TextEdit, Uri};

use crate::client::{Client, LspError, ResponseError, ServerHandler};
use crate::convert;
use crate::encoding::{Encoding, range_from_lsp, range_to_lsp, to_lsp};
use crate::types::{CodeAction, CompletionItem, Diagnostic, Hover, Location, WorkspaceEdit};

mod navigation;

/// Restarts of a crashing server before it stays stopped.
pub const MAX_RESTARTS: u32 = 3;
/// How long a request waits for a starting server.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a request waits for edits still on their way.
const CATCH_UP: Duration = Duration::from_millis(500);

/// A language server a plugin contributes (`[[contributes.languageServers]]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDecl {
    pub id: String,
    pub plugin: String,
    pub languages: Vec<String>,
}

/// One running instance: a contributed server for one workspace folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerKey {
    pub id: String,
    pub root: Uri,
}

/// What the service needs from the rest of the core.
pub trait LanguageHost: Send + Sync + 'static {
    /// The server's command line, from the plugin that contributes it.
    fn resolve_launch(
        &self,
        server: &ServerDecl,
        language: &str,
        root: &Uri,
    ) -> BoxFuture<'static, Result<LaunchSpec, String>>;
    /// Applies edits through the Documents service; `false` when some could not be applied.
    fn apply_edit(&self, edit: WorkspaceEdit) -> BoxFuture<'static, bool>;
    /// Text of a file that is not open, to convert a server's positions in it.
    fn read_text(&self, uri: &Uri) -> BoxFuture<'static, Option<String>>;
    /// Diagnostics of these files changed.
    fn diagnostics_changed(&self, uris: Vec<Uri>);
    /// A server's log line (`window/logMessage`, stderr).
    fn log(&self, server: &str, message: &str);
    fn workspace_folders(&self) -> Vec<Uri>;
    /// Servers started, stopped or reported progress.
    fn status_changed(&self);
    /// The version an open document has now; the service may still be catching up with it.
    fn document_version(&self, uri: &Uri) -> Option<u64>;
    /// LSP methods the plugin contributing `server` intercepts (`lspMiddleware`).
    fn middleware(&self, server: &ServerDecl) -> Vec<String>;
    /// Hands an intercepted message to the plugin (`lsp/middleware/request`).
    fn intercept(
        &self,
        server: &ServerDecl,
        method: &str,
        params: Value,
    ) -> BoxFuture<'static, Result<MiddlewareResult, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    Starting,
    Running,
    Stopped(String),
}

struct Doc {
    language: String,
    version: i32,
    rope: Rope,
}

/// Changes not yet sent to one server for one document.
#[derive(Default)]
struct Pending {
    changes: Vec<lsp_types::TextDocumentContentChangeEvent>,
    /// A full-text sync server gets the text at flush time instead.
    full: bool,
    scheduled: bool,
}

struct Running {
    client: Client,
    encoding: Encoding,
    capabilities: lsp_types::ServerCapabilities,
    sync: lsp_types::TextDocumentSyncKind,
    control: Box<dyn ProcessControl>,
    pid: u32,
    opened: Mutex<HashSet<Uri>>,
    pending: Mutex<HashMap<Uri, Pending>>,
}

#[derive(Clone)]
enum Status {
    Starting,
    Running(Arc<Running>),
    Stopped(String),
}

struct Server {
    key: ServerKey,
    decl: ServerDecl,
    language: String,
    status: watch::Sender<Status>,
    restarts: Mutex<u32>,
}

/// What a server reports doing (`$/progress`, work done progress).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub server: String,
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u32>,
}

/// A server's diagnostics for one file: ours, and the raw ones for `codeAction` context.
type FileDiagnostics = Vec<(ServerKey, Vec<(Diagnostic, lsp_types::Diagnostic)>)>;

#[derive(Default)]
struct State {
    decls: Vec<ServerDecl>,
    docs: HashMap<Uri, Doc>,
    servers: HashMap<ServerKey, Arc<Server>>,
    diagnostics: HashMap<Uri, FileDiagnostics>,
    /// Work in progress (`$/progress`) by server and token.
    progress: Vec<(ServerKey, String, Progress)>,
    shut_down: bool,
}

struct Shared {
    processes: Arc<dyn Processes>,
    host: Arc<dyn LanguageHost>,
    state: Mutex<State>,
    debounce: Duration,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
pub struct LanguageService {
    shared: Arc<Shared>,
}

impl LanguageService {
    pub fn new(processes: Arc<dyn Processes>, host: Arc<dyn LanguageHost>) -> Self {
        Self::with_debounce(processes, host, Duration::from_millis(50))
    }

    pub fn with_debounce(
        processes: Arc<dyn Processes>,
        host: Arc<dyn LanguageHost>,
        debounce: Duration,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                processes,
                host,
                state: Mutex::new(State::default()),
                debounce,
            }),
        }
    }

    /// The servers plugins contribute; servers no longer contributed are stopped.
    pub fn set_servers(&self, decls: Vec<ServerDecl>) {
        let stopped: Vec<Arc<Server>> = {
            let mut state = lock(&self.shared.state);
            state.decls = decls;
            let keep: Vec<ServerKey> = state
                .servers
                .iter()
                .filter(|(_, s)| state.decls.contains(&s.decl))
                .map(|(k, _)| k.clone())
                .collect();
            let gone: Vec<ServerKey> = state
                .servers
                .keys()
                .filter(|k| !keep.contains(k))
                .cloned()
                .collect();
            gone.iter()
                .filter_map(|k| state.servers.remove(k))
                .collect()
        };
        for server in stopped {
            self.shared.clear_diagnostics(&server.key);
            tokio::spawn(stop_server(server));
        }
        // Open documents may have a server now.
        let docs: Vec<(Uri, String)> = lock(&self.shared.state)
            .docs
            .iter()
            .map(|(uri, doc)| (uri.clone(), doc.language.clone()))
            .collect();
        for (uri, language) in docs {
            self.shared.ensure_servers(&uri, &language);
        }
    }

    pub fn servers(&self) -> Vec<(ServerKey, ServerStatus)> {
        let state = lock(&self.shared.state);
        let mut list: Vec<_> = state
            .servers
            .values()
            .map(|s| {
                let status = match &*s.status.borrow() {
                    Status::Starting => ServerStatus::Starting,
                    Status::Running(_) => ServerStatus::Running,
                    Status::Stopped(reason) => ServerStatus::Stopped(reason.clone()),
                };
                (s.key.clone(), status)
            })
            .collect();
        list.sort_by(|a, b| (&a.0.id, a.0.root.as_str()).cmp(&(&b.0.id, b.0.root.as_str())));
        list
    }

    /// Process id of a running server, for tests and diagnostics.
    pub fn server_pid(&self, key: &ServerKey) -> Option<u32> {
        let server = lock(&self.shared.state).servers.get(key).cloned()?;
        let status = server.status.borrow().clone();
        match status {
            Status::Running(running) => Some(running.pid),
            _ => None,
        }
    }

    /// What the servers report doing, oldest first.
    pub fn progress(&self) -> Vec<Progress> {
        lock(&self.shared.state)
            .progress
            .iter()
            .map(|(_, _, p)| p.clone())
            .collect()
    }

    // ---- document sync ------------------------------------------------------------------

    pub fn did_open(&self, uri: &Uri, language: &str, version: u64, text: Rope) {
        {
            let mut state = lock(&self.shared.state);
            if state.shut_down {
                return;
            }
            let doc = Doc {
                language: language.to_owned(),
                version: lsp_version(version),
                rope: text,
            };
            state.docs.insert(uri.clone(), doc);
            let running: Vec<Arc<Running>> = state
                .servers_for(uri, language)
                .into_iter()
                .filter_map(|s| s.running())
                .collect();
            for running in running {
                running.open(uri, &state.docs[uri]);
            }
        }
        self.shared.ensure_servers(uri, language);
    }

    /// A transaction: edits in application order, each relative to the text after the
    /// previous one, and the version after it.
    pub fn did_change(&self, uri: &Uri, version: u64, edits: &[TextEdit]) {
        let mut state = lock(&self.shared.state);
        let State { docs, servers, .. } = &mut *state;
        let Some(doc) = docs.get_mut(uri) else {
            return;
        };
        // Opened from a snapshot that already had this change.
        if lsp_version(version) <= doc.version {
            return;
        }
        let running: Vec<Arc<Running>> = servers
            .values()
            .filter_map(|s| s.running())
            .filter(|r| lock(&r.opened).contains(uri))
            .collect();
        for edit in edits {
            for server in &running {
                server.queue_change(uri, &doc.rope, edit);
            }
            apply(&mut doc.rope, edit);
        }
        doc.version = lsp_version(version);
        for server in running {
            self.shared.schedule_flush(&server, uri);
        }
    }

    pub fn did_save(&self, uri: &Uri) {
        let state = lock(&self.shared.state);
        let Some(doc) = state.docs.get(uri) else {
            return;
        };
        for server in state.running_with(uri) {
            server.flush(uri, doc);
            server
                .client
                .notify::<lsp_types::notification::DidSaveTextDocument>(
                    lsp_types::DidSaveTextDocumentParams {
                        text_document: lsp_types::TextDocumentIdentifier {
                            uri: convert::uri_to_lsp(uri),
                        },
                        text: None,
                    },
                );
        }
    }

    pub fn did_close(&self, uri: &Uri) {
        let mut state = lock(&self.shared.state);
        for server in state.running_with(uri) {
            if let Some(doc) = state.docs.get(uri) {
                server.flush(uri, doc);
            }
            lock(&server.opened).remove(uri);
            server
                .client
                .notify::<lsp_types::notification::DidCloseTextDocument>(
                    lsp_types::DidCloseTextDocumentParams {
                        text_document: lsp_types::TextDocumentIdentifier {
                            uri: convert::uri_to_lsp(uri),
                        },
                    },
                );
        }
        state.docs.remove(uri);
    }

    // ---- diagnostics --------------------------------------------------------------------

    /// Diagnostics of a file from all its servers, by position.
    pub fn diagnostics(&self, uri: &Uri) -> Vec<Diagnostic> {
        let state = lock(&self.shared.state);
        let mut list: Vec<Diagnostic> = state
            .diagnostics
            .get(uri)
            .into_iter()
            .flatten()
            .flat_map(|(_, list)| list.iter().map(|(d, _)| d.clone()))
            .collect();
        list.sort_by_key(|d| (d.range.start, d.severity));
        list
    }

    /// Every file with diagnostics.
    pub fn all_diagnostics(&self) -> Vec<(Uri, Vec<Diagnostic>)> {
        let uris: Vec<Uri> = lock(&self.shared.state)
            .diagnostics
            .keys()
            .cloned()
            .collect();
        let mut all: Vec<_> = uris
            .into_iter()
            .map(|uri| {
                let list = self.diagnostics(&uri);
                (uri, list)
            })
            .filter(|(_, list)| !list.is_empty())
            .collect();
        all.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        all
    }

    // ---- features -----------------------------------------------------------------------

    /// Completion from every server of the document that offers it, merged by sort text.
    pub async fn completion(&self, uri: &Uri, position: Position) -> Vec<CompletionItem> {
        let servers = self
            .shared
            .ready(uri, |c| c.completion_provider.is_some())
            .await;
        let mut items = Vec::new();
        for (key, server) in servers {
            let Some(params) =
                self.shared
                    .with_doc(uri, &server, |doc| lsp_types::CompletionParams {
                        text_document_position: position_params(
                            uri,
                            doc,
                            server.encoding,
                            position,
                        ),
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                        context: None,
                    })
            else {
                continue;
            };
            let response = match server
                .client
                .request::<lsp_types::request::Completion>(params)
                .await
            {
                Ok(Some(response)) => response,
                Ok(None) => continue,
                Err(error) => {
                    self.shared
                        .host
                        .log(&key.id, &format!("completion: {error}"));
                    continue;
                }
            };
            let raw = match response {
                lsp_types::CompletionResponse::Array(items) => items,
                lsp_types::CompletionResponse::List(list) => list.items,
            };
            let resolvable = server
                .capabilities
                .completion_provider
                .as_ref()
                .and_then(|c| c.resolve_provider)
                .unwrap_or(false);
            self.shared.with_doc(uri, &server, |doc| {
                for raw in &raw {
                    let mut item = convert::completion_item(&doc.rope, server.encoding, raw);
                    if resolvable {
                        item.data = serde_json::to_value(raw).ok().map(|v| (key.clone(), v));
                    }
                    items.push(item);
                }
            });
        }
        items.sort_by(|a, b| a.sort_text.cmp(&b.sort_text));
        items
    }

    /// Fills in what a server left for `completionItem/resolve` (e.g. imports to add).
    pub async fn resolve_completion(&self, uri: &Uri, item: CompletionItem) -> CompletionItem {
        let Some((key, raw)) = item.data.clone() else {
            return item;
        };
        let Some(server) = self.shared.running(&key) else {
            return item;
        };
        let Ok(raw) = serde_json::from_value::<lsp_types::CompletionItem>(raw) else {
            return item;
        };
        let resolved = match server
            .client
            .request::<lsp_types::request::ResolveCompletionItem>(raw)
            .await
        {
            Ok(resolved) => resolved,
            Err(error) => {
                self.shared
                    .host
                    .log(&key.id, &format!("completion resolve: {error}"));
                return item;
            }
        };
        self.shared
            .with_doc(uri, &server, |doc| {
                let mut full = convert::completion_item(&doc.rope, server.encoding, &resolved);
                full.data = None;
                full
            })
            .unwrap_or(item)
    }

    pub async fn hover(&self, uri: &Uri, position: Position) -> Option<Hover> {
        let servers = self.shared.ready(uri, |c| c.hover_provider.is_some()).await;
        for (key, server) in servers {
            let params = self
                .shared
                .with_doc(uri, &server, |doc| lsp_types::HoverParams {
                    text_document_position_params: position_params(
                        uri,
                        doc,
                        server.encoding,
                        position,
                    ),
                    work_done_progress_params: Default::default(),
                })?;
            match server
                .client
                .request::<lsp_types::request::HoverRequest>(params)
                .await
            {
                Ok(Some(hover)) => {
                    return self.shared.with_doc(uri, &server, |doc| {
                        convert::hover(&doc.rope, server.encoding, hover)
                    });
                }
                Ok(None) => {}
                Err(error) => self.shared.host.log(&key.id, &format!("hover: {error}")),
            }
        }
        None
    }

    /// Where the symbol at `position` is defined; locations of all servers, deduplicated.
    pub async fn definition(&self, uri: &Uri, position: Position) -> Vec<Location> {
        let servers = self
            .shared
            .ready(uri, |c| c.definition_provider.is_some())
            .await;
        let mut found = Vec::new();
        for (key, server) in servers {
            let Some(params) =
                self.shared
                    .with_doc(uri, &server, |doc| lsp_types::GotoDefinitionParams {
                        text_document_position_params: position_params(
                            uri,
                            doc,
                            server.encoding,
                            position,
                        ),
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    })
            else {
                continue;
            };
            let response = match server
                .client
                .request::<lsp_types::request::GotoDefinition>(params)
                .await
            {
                Ok(Some(response)) => response,
                Ok(None) => continue,
                Err(error) => {
                    self.shared
                        .host
                        .log(&key.id, &format!("definition: {error}"));
                    continue;
                }
            };
            let raw: Vec<(lsp_types::Uri, lsp_types::Range)> = match response {
                lsp_types::GotoDefinitionResponse::Scalar(l) => vec![(l.uri, l.range)],
                lsp_types::GotoDefinitionResponse::Array(list) => {
                    list.into_iter().map(|l| (l.uri, l.range)).collect()
                }
                lsp_types::GotoDefinitionResponse::Link(links) => links
                    .into_iter()
                    .map(|l| (l.target_uri, l.target_selection_range))
                    .collect(),
            };
            for (target, range) in raw {
                let Some(target) = convert::uri_from_lsp(&target) else {
                    continue;
                };
                let range = self
                    .shared
                    .convert_range(&target, server.encoding, range)
                    .await;
                let location = Location { uri: target, range };
                if !found.contains(&location) {
                    found.push(location);
                }
            }
        }
        found
    }

    /// Fixes and refactorings for a range, with the diagnostics there as context.
    pub async fn code_actions(&self, uri: &Uri, range: Range) -> Vec<CodeAction> {
        let servers = self
            .shared
            .ready(uri, |c| c.code_action_provider.is_some())
            .await;
        let mut actions = Vec::new();
        for (key, server) in servers {
            let diagnostics: Vec<lsp_types::Diagnostic> = {
                let state = lock(&self.shared.state);
                state
                    .diagnostics
                    .get(uri)
                    .into_iter()
                    .flatten()
                    .filter(|(k, _)| *k == key)
                    .flat_map(|(_, list)| list.iter())
                    .filter(|(d, _)| d.range.start <= range.end && range.start <= d.range.end)
                    .map(|(_, raw)| raw.clone())
                    .collect()
            };
            let Some(params) =
                self.shared
                    .with_doc(uri, &server, |doc| lsp_types::CodeActionParams {
                        text_document: lsp_types::TextDocumentIdentifier {
                            uri: convert::uri_to_lsp(uri),
                        },
                        range: range_to_lsp(&doc.rope, server.encoding, range),
                        context: lsp_types::CodeActionContext {
                            diagnostics,
                            only: None,
                            trigger_kind: Some(lsp_types::CodeActionTriggerKind::INVOKED),
                        },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    })
            else {
                continue;
            };
            let response = match server
                .client
                .request::<lsp_types::request::CodeActionRequest>(params)
                .await
            {
                Ok(Some(response)) => response,
                Ok(None) => continue,
                Err(error) => {
                    self.shared
                        .host
                        .log(&key.id, &format!("code actions: {error}"));
                    continue;
                }
            };
            for entry in response {
                let (title, kind, preferred, disabled) = match &entry {
                    lsp_types::CodeActionOrCommand::Command(command) => {
                        (command.title.clone(), None, false, false)
                    }
                    lsp_types::CodeActionOrCommand::CodeAction(action) => (
                        action.title.clone(),
                        action.kind.as_ref().map(|k| k.as_str().to_owned()),
                        action.is_preferred.unwrap_or(false),
                        action.disabled.is_some(),
                    ),
                };
                if disabled {
                    continue;
                }
                let Ok(raw) = serde_json::to_value(&entry) else {
                    continue;
                };
                actions.push(CodeAction {
                    title,
                    kind,
                    preferred,
                    server: key.clone(),
                    raw,
                });
            }
        }
        // Preferred fixes first, as IntelliJ lists quick fixes before other intentions.
        actions.sort_by_key(|a| !a.preferred);
        actions
    }

    /// Applies an action: resolves it if the server wants that, applies its edit, runs its
    /// command (which may come back as `workspace/applyEdit`).
    pub async fn apply_code_action(&self, action: &CodeAction) -> Result<(), String> {
        let server = self
            .shared
            .running(&action.server)
            .ok_or_else(|| format!("{} is not running", action.server.id))?;
        let entry: lsp_types::CodeActionOrCommand =
            serde_json::from_value(action.raw.clone()).map_err(|e| e.to_string())?;
        let (edit, command) = match entry {
            lsp_types::CodeActionOrCommand::Command(command) => (None, Some(command)),
            lsp_types::CodeActionOrCommand::CodeAction(mut code_action) => {
                let resolvable = matches!(
                    &server.capabilities.code_action_provider,
                    Some(lsp_types::CodeActionProviderCapability::Options(o))
                        if o.resolve_provider == Some(true)
                );
                if code_action.edit.is_none() && resolvable {
                    code_action = server
                        .client
                        .request::<lsp_types::request::CodeActionResolveRequest>(code_action)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                (code_action.edit, code_action.command)
            }
        };
        if let Some(edit) = edit {
            let edit = self.shared.workspace_edit(server.encoding, &edit).await;
            if !self.shared.host.apply_edit(edit).await {
                return Err("the edit could not be applied".into());
            }
        }
        if let Some(command) = command {
            let mut params = serde_json::to_value(lsp_types::ExecuteCommandParams {
                command: command.command,
                arguments: command.arguments.unwrap_or_default(),
                work_done_progress_params: Default::default(),
            })
            .map_err(|e| e.to_string())?;
            // Client-side commands (e.g. a server asking the editor to apply something) belong
            // to the language plugin when it intercepts executeCommand.
            let method = "workspace/executeCommand";
            let decl = lock(&self.shared.state)
                .servers
                .get(&action.server)
                .map(|s| s.decl.clone());
            if let Some(decl) = decl
                && self
                    .shared
                    .host
                    .middleware(&decl)
                    .iter()
                    .any(|m| m == method)
            {
                match self.shared.host.intercept(&decl, method, params).await? {
                    MiddlewareResult::Respond { .. } => return Ok(()),
                    MiddlewareResult::Forward { params: forwarded } => params = forwarded,
                }
            }
            server
                .client
                .request_value(method, &params)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// A raw request to a running server (`lsp/sendRequest` from its plugin).
    pub async fn send_request(
        &self,
        plugin: &str,
        server_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, LspError> {
        let running: Vec<Arc<Running>> = lock(&self.shared.state)
            .servers
            .values()
            .filter(|s| s.key.id == server_id && s.decl.plugin == plugin)
            .filter_map(|s| s.running())
            .collect();
        let server = running.first().ok_or(LspError::Closed)?;
        server.client.request_value(method, &params).await
    }

    /// Applies an LSP workspace edit of a server the plugin contributes (`lsp/applyEdit`).
    pub async fn apply_server_edit(
        &self,
        plugin: &str,
        server_id: &str,
        edit: Value,
    ) -> Result<bool, String> {
        let encoding = lock(&self.shared.state)
            .servers
            .values()
            .filter(|s| s.key.id == server_id && s.decl.plugin == plugin)
            .find_map(|s| s.running())
            .map(|r| r.encoding)
            .ok_or_else(|| format!("{server_id} is not a running server of {plugin}"))?;
        let edit: lsp_types::WorkspaceEdit =
            serde_json::from_value(edit).map_err(|e| e.to_string())?;
        let edit = self.shared.workspace_edit(encoding, &edit).await;
        Ok(self.shared.host.apply_edit(edit).await)
    }

    /// Stops every server politely (`shutdown`, `exit`), killing what does not stop in time.
    pub async fn shutdown(&self) {
        let servers: Vec<Arc<Server>> = {
            let mut state = lock(&self.shared.state);
            state.shut_down = true;
            state.servers.drain().map(|(_, s)| s).collect()
        };
        let stops = servers.into_iter().map(stop_server);
        for stop in stops {
            stop.await;
        }
    }
}

/// LSP versions are `i32`; ours only grow, so saturate.
fn lsp_version(version: u64) -> i32 {
    i32::try_from(version).unwrap_or(i32::MAX)
}

fn char_of(rope: &Rope, position: Position) -> usize {
    let line = (position.line as usize).min(rope.len_lines().saturating_sub(1));
    let start = rope.line_to_byte(line);
    let byte = (start + position.character as usize).min(rope.len_bytes());
    rope.byte_to_char(byte)
}

fn apply(rope: &mut Rope, edit: &TextEdit) {
    let start = char_of(rope, edit.range.start);
    let end = char_of(rope, edit.range.end).max(start);
    rope.remove(start..end);
    rope.insert(start, &edit.new_text);
}

fn position_params(
    uri: &Uri,
    doc: &Doc,
    encoding: Encoding,
    position: Position,
) -> lsp_types::TextDocumentPositionParams {
    lsp_types::TextDocumentPositionParams {
        text_document: lsp_types::TextDocumentIdentifier {
            uri: convert::uri_to_lsp(uri),
        },
        position: to_lsp(&doc.rope, encoding, position),
    }
}

impl Running {
    fn open(&self, uri: &Uri, doc: &Doc) {
        if !lock(&self.opened).insert(uri.clone()) {
            return;
        }
        self.client
            .notify::<lsp_types::notification::DidOpenTextDocument>(
                lsp_types::DidOpenTextDocumentParams {
                    text_document: lsp_types::TextDocumentItem {
                        uri: convert::uri_to_lsp(uri),
                        language_id: doc.language.clone(),
                        version: doc.version,
                        text: doc.rope.to_string(),
                    },
                },
            );
    }

    /// Converts an edit against the text before it (the server's view) and queues it.
    fn queue_change(&self, uri: &Uri, before: &Rope, edit: &TextEdit) {
        let mut pending = lock(&self.pending);
        let entry = pending.entry(uri.clone()).or_default();
        match self.sync {
            lsp_types::TextDocumentSyncKind::INCREMENTAL => {
                entry
                    .changes
                    .push(lsp_types::TextDocumentContentChangeEvent {
                        range: Some(range_to_lsp(before, self.encoding, edit.range)),
                        range_length: None,
                        text: edit.new_text.clone(),
                    });
            }
            lsp_types::TextDocumentSyncKind::FULL => entry.full = true,
            _ => {}
        }
    }

    /// Sends what is queued for the document.
    fn flush(&self, uri: &Uri, doc: &Doc) {
        let Some(mut pending) = lock(&self.pending).remove(uri) else {
            return;
        };
        if pending.full {
            pending.changes = vec![lsp_types::TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: doc.rope.to_string(),
            }];
        }
        if pending.changes.is_empty() {
            return;
        }
        self.client
            .notify::<lsp_types::notification::DidChangeTextDocument>(
                lsp_types::DidChangeTextDocumentParams {
                    text_document: lsp_types::VersionedTextDocumentIdentifier {
                        uri: convert::uri_to_lsp(uri),
                        version: doc.version,
                    },
                    content_changes: pending.changes,
                },
            );
    }
}

impl Server {
    fn running(&self) -> Option<Arc<Running>> {
        match &*self.status.borrow() {
            Status::Running(running) => Some(running.clone()),
            _ => None,
        }
    }
}

impl State {
    /// Servers (existing ones) that serve this document.
    fn servers_for(&self, uri: &Uri, language: &str) -> Vec<Arc<Server>> {
        self.servers
            .values()
            .filter(|s| s.decl.languages.iter().any(|l| l == language))
            .filter(|s| is_under(uri, &s.key.root))
            .cloned()
            .collect()
    }

    fn running_with(&self, uri: &Uri) -> Vec<Arc<Running>> {
        self.servers
            .values()
            .filter_map(|s| s.running())
            .filter(|r| lock(&r.opened).contains(uri))
            .collect()
    }
}

fn is_under(uri: &Uri, root: &Uri) -> bool {
    let root = root.as_str().trim_end_matches('/');
    uri.as_str()
        .strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

impl Shared {
    /// The folder a document's server works in: the innermost workspace folder containing
    /// it, or the file's directory.
    fn root_for(&self, uri: &Uri) -> Option<Uri> {
        self.host
            .workspace_folders()
            .into_iter()
            .filter(|folder| is_under(uri, folder))
            .max_by_key(|folder| folder.as_str().len())
            .or_else(|| {
                let parent = uri.to_file_path()?.parent()?.to_path_buf();
                Uri::from_file_path(parent).ok()
            })
    }

    /// Creates and starts the servers a document of `language` needs.
    fn ensure_servers(self: &Arc<Self>, uri: &Uri, language: &str) {
        let Some(root) = self.root_for(uri) else {
            return;
        };
        let mut state = lock(&self.state);
        if state.shut_down {
            return;
        }
        let decls: Vec<ServerDecl> = state
            .decls
            .iter()
            .filter(|d| d.languages.iter().any(|l| l == language))
            .cloned()
            .collect();
        for decl in decls {
            let key = ServerKey {
                id: decl.id.clone(),
                root: root.clone(),
            };
            if state.servers.contains_key(&key) {
                continue;
            }
            let (status, _) = watch::channel(Status::Starting);
            let server = Arc::new(Server {
                key: key.clone(),
                decl,
                language: language.to_owned(),
                status,
                restarts: Mutex::new(0),
            });
            state.servers.insert(key, server.clone());
            tokio::spawn(start(Arc::downgrade(self), server));
        }
    }

    fn running(&self, key: &ServerKey) -> Option<Arc<Running>> {
        lock(&self.state).servers.get(key)?.running()
    }

    /// The document's servers whose capabilities pass `wanted`, waiting for those still
    /// starting; pending edits are sent first so the server answers about the current text.
    async fn ready(
        &self,
        uri: &Uri,
        wanted: impl Fn(&lsp_types::ServerCapabilities) -> bool,
    ) -> Vec<(ServerKey, Arc<Running>)> {
        self.catch_up(uri).await;
        let servers: Vec<Arc<Server>> = {
            let state = lock(&self.state);
            let Some(doc) = state.docs.get(uri) else {
                return Vec::new();
            };
            state.servers_for(uri, &doc.language)
        };
        let mut ready = Vec::new();
        for server in servers {
            let mut status = server.status.subscribe();
            let settled = tokio::time::timeout(
                START_TIMEOUT,
                status.wait_for(|s| !matches!(s, Status::Starting)),
            )
            .await;
            let running = match settled {
                Ok(Ok(status)) => match &*status {
                    Status::Running(running) => running.clone(),
                    _ => continue,
                },
                _ => continue,
            };
            if wanted(&running.capabilities) {
                ready.push((server.key.clone(), running));
            }
        }
        let state = lock(&self.state);
        if let Some(doc) = state.docs.get(uri) {
            for (_, running) in &ready {
                running.open(uri, doc);
                running.flush(uri, doc);
            }
        }
        ready
    }

    /// Waits (briefly) until the edits of the document have arrived as events, so a request
    /// right after an edit is about the edited text.
    async fn catch_up(&self, uri: &Uri) {
        let Some(wanted) = self.host.document_version(uri).map(lsp_version) else {
            return;
        };
        let deadline = tokio::time::Instant::now() + CATCH_UP;
        loop {
            let known = lock(&self.state).docs.get(uri).map(|d| d.version);
            if known.is_none_or(|v| v >= wanted) || tokio::time::Instant::now() > deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    fn with_doc<T>(&self, uri: &Uri, _server: &Running, f: impl FnOnce(&Doc) -> T) -> Option<T> {
        lock(&self.state).docs.get(uri).map(f)
    }

    fn schedule_flush(self: &Arc<Self>, server: &Arc<Running>, uri: &Uri) {
        {
            let mut pending = lock(&server.pending);
            let Some(entry) = pending.get_mut(uri) else {
                return;
            };
            if entry.scheduled {
                return;
            }
            entry.scheduled = true;
        }
        let shared = Arc::downgrade(self);
        let server = Arc::downgrade(server);
        let uri = uri.clone();
        let delay = self.debounce;
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let (Some(shared), Some(server)) = (shared.upgrade(), server.upgrade()) else {
                return;
            };
            let state = lock(&shared.state);
            if let Some(doc) = state.docs.get(&uri) {
                server.flush(&uri, doc);
            }
        });
    }

    /// A range in `uri`, converted against its open text or the file on disk.
    async fn convert_range(&self, uri: &Uri, encoding: Encoding, range: lsp_types::Range) -> Range {
        if let Some(converted) = lock(&self.state)
            .docs
            .get(uri)
            .map(|doc| range_from_lsp(&doc.rope, encoding, range))
        {
            return converted;
        }
        let text = match encoding {
            Encoding::Utf8 => None,
            Encoding::Utf16 => self.host.read_text(uri).await,
        };
        let rope = text.map(|t| Rope::from_str(&t)).unwrap_or_default();
        if rope.len_chars() == 0 {
            // Nothing to convert against: take the numbers as they are.
            return Range::new(
                Position::new(range.start.line, range.start.character),
                Position::new(range.end.line, range.end.character),
            );
        }
        range_from_lsp(&rope, encoding, range)
    }

    async fn workspace_edit(
        &self,
        encoding: Encoding,
        edit: &lsp_types::WorkspaceEdit,
    ) -> WorkspaceEdit {
        let (parts, unsupported) = convert::workspace_edit_parts(edit);
        for operation in unsupported {
            self.host.log(
                "language",
                &format!("unsupported edit operation {operation}"),
            );
        }
        let mut changes = Vec::new();
        for (uri, edits) in parts {
            let Some(uri) = convert::uri_from_lsp(&uri) else {
                continue;
            };
            let open = lock(&self.state).docs.get(&uri).map(|d| d.rope.clone());
            let rope = match open {
                Some(rope) => rope,
                None => match encoding {
                    Encoding::Utf8 => Rope::new(),
                    Encoding::Utf16 => self
                        .host
                        .read_text(&uri)
                        .await
                        .map(|t| Rope::from_str(&t))
                        .unwrap_or_default(),
                },
            };
            let converted = edits
                .iter()
                .map(|e| {
                    if rope.len_chars() == 0 && encoding == Encoding::Utf8 {
                        TextEdit {
                            range: Range::new(
                                Position::new(e.range.start.line, e.range.start.character),
                                Position::new(e.range.end.line, e.range.end.character),
                            ),
                            new_text: e.new_text.clone(),
                        }
                    } else {
                        convert::text_edit(&rope, encoding, e)
                    }
                })
                .collect();
            changes.push((uri, converted));
        }
        WorkspaceEdit { changes }
    }

    fn set_diagnostics(
        &self,
        key: &ServerKey,
        uri: Uri,
        list: Vec<(Diagnostic, lsp_types::Diagnostic)>,
    ) {
        {
            let mut state = lock(&self.state);
            let file = state.diagnostics.entry(uri.clone()).or_default();
            file.retain(|(k, _)| k != key);
            if !list.is_empty() {
                file.push((key.clone(), list));
            }
            if file.is_empty() {
                state.diagnostics.remove(&uri);
            }
        }
        self.host.diagnostics_changed(vec![uri]);
    }

    fn clear_diagnostics(&self, key: &ServerKey) {
        let changed: Vec<Uri> = {
            let mut state = lock(&self.state);
            let mut changed = Vec::new();
            state.diagnostics.retain(|uri, file| {
                let before = file.len();
                file.retain(|(k, _)| k != key);
                if file.len() != before {
                    changed.push(uri.clone());
                }
                !file.is_empty()
            });
            changed
        };
        if !changed.is_empty() {
            self.host.diagnostics_changed(changed);
        }
    }
}

/// Starts (or restarts) a server and brings it its documents.
fn start(shared: Weak<Shared>, server: Arc<Server>) -> BoxFuture<'static, ()> {
    Box::pin(start_inner(shared, server))
}

async fn start_inner(shared: Weak<Shared>, server: Arc<Server>) {
    let Some(strong) = shared.upgrade() else {
        return;
    };
    let result = launch(&strong, &shared, &server).await;
    let running = match result {
        Ok(running) => running,
        Err(error) => {
            strong
                .host
                .log(&server.key.id, &format!("cannot start: {error}"));
            let _ = server.status.send_replace(Status::Stopped(error));
            strong.host.status_changed();
            return;
        }
    };
    {
        let state = lock(&strong.state);
        let still_wanted = state
            .servers
            .get(&server.key)
            .is_some_and(|s| Arc::ptr_eq(s, &server));
        if !still_wanted || state.shut_down {
            drop(state);
            running.control.kill();
            return;
        }
        let _ = server.status.send_replace(Status::Running(running.clone()));
        for (uri, doc) in &state.docs {
            if server.decl.languages.contains(&doc.language) && is_under(uri, &server.key.root) {
                running.open(uri, doc);
            }
        }
    }
    strong.host.status_changed();
    drop(strong);
    tokio::spawn(watch_exit(shared, server, running));
}

async fn launch(
    shared: &Arc<Shared>,
    weak: &Weak<Shared>,
    server: &Arc<Server>,
) -> Result<Arc<Running>, String> {
    let launch = shared
        .host
        .resolve_launch(&server.decl, &server.language, &server.key.root)
        .await?;
    let cwd = launch
        .cwd
        .map(std::path::PathBuf::from)
        .or_else(|| server.key.root.to_file_path());
    let process = shared
        .processes
        .spawn(ProcessSpec {
            command: launch.command,
            args: launch.args,
            cwd,
            env: launch.env,
            owner: server.decl.plugin.clone(),
        })
        .map_err(|e| e.to_string())?;

    // stderr goes to the log, line by line.
    let host = shared.host.clone();
    let id = server.key.id.clone();
    let stderr = process.stderr;
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            host.log(&id, &line);
        }
    });

    let handler = Arc::new(Handler {
        shared: weak.clone(),
        key: server.key.clone(),
        encoding: Mutex::new(Encoding::Utf16),
    });
    let client = Client::start(process.stdin, process.stdout, handler.clone());
    let pid = process.pid;
    let folders: Vec<lsp_types::WorkspaceFolder> = vec![lsp_types::WorkspaceFolder {
        uri: convert::uri_to_lsp(&server.key.root),
        name: server
            .key
            .root
            .to_file_path()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| server.key.root.to_string()),
    }];
    #[allow(deprecated)] // root_uri: older servers still read it
    let params = lsp_types::InitializeParams {
        process_id: Some(std::process::id()),
        root_uri: Some(convert::uri_to_lsp(&server.key.root)),
        initialization_options: launch.initialization_options,
        capabilities: client_capabilities(),
        workspace_folders: Some(folders),
        client_info: Some(lsp_types::ClientInfo {
            name: "Kumiko".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
        ..Default::default()
    };
    let initialized = tokio::time::timeout(
        START_TIMEOUT,
        client.request::<lsp_types::request::Initialize>(params),
    )
    .await;
    let result = match initialized {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            process.control.kill();
            return Err(format!("initialize failed: {error}"));
        }
        Err(_) => {
            process.control.kill();
            return Err("initialize timed out".into());
        }
    };
    client.notify::<lsp_types::notification::Initialized>(lsp_types::InitializedParams {});
    let encoding = Encoding::negotiated(&result.capabilities);
    *lock(&handler.encoding) = encoding;
    let sync = match &result.capabilities.text_document_sync {
        Some(lsp_types::TextDocumentSyncCapability::Kind(kind)) => *kind,
        Some(lsp_types::TextDocumentSyncCapability::Options(options)) => options
            .change
            .unwrap_or(lsp_types::TextDocumentSyncKind::NONE),
        None => lsp_types::TextDocumentSyncKind::NONE,
    };
    tracing::info!(server = %server.key.id, root = %server.key.root, pid, ?encoding, "language server ready");
    Ok(Arc::new(Running {
        client,
        encoding,
        capabilities: result.capabilities,
        sync,
        control: process.control,
        pid,
        opened: Mutex::new(HashSet::new()),
        pending: Mutex::new(HashMap::new()),
    }))
}

/// Restarts a server that exits on its own.
async fn watch_exit(shared: Weak<Shared>, server: Arc<Server>, running: Arc<Running>) {
    running.client.wait_closed().await;
    let Some(strong) = shared.upgrade() else {
        return;
    };
    let current =
        matches!(&*server.status.borrow(), Status::Running(r) if Arc::ptr_eq(r, &running));
    if !current {
        return; // stopped on purpose
    }
    strong.clear_diagnostics(&server.key);
    lock(&strong.state)
        .progress
        .retain(|(k, _, _)| *k != server.key);
    let attempt = {
        let mut restarts = lock(&server.restarts);
        *restarts += 1;
        *restarts
    };
    if attempt > MAX_RESTARTS {
        strong
            .host
            .log(&server.key.id, "exited too often, not restarting");
        let _ = server
            .status
            .send_replace(Status::Stopped("exited too often".into()));
        strong.host.status_changed();
        return;
    }
    strong.host.log(
        &server.key.id,
        &format!("exited, restarting (attempt {attempt})"),
    );
    let _ = server.status.send_replace(Status::Starting);
    strong.host.status_changed();
    drop(strong);
    tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
    start(shared, server).await;
}

async fn stop_server(server: Arc<Server>) {
    let previous = server
        .status
        .send_replace(Status::Stopped("stopped".into()));
    let Status::Running(running) = previous else {
        return;
    };
    let polite = async {
        let _ = running.client.request_value("shutdown", &Value::Null).await;
        running.client.notify_value("exit", &Value::Null);
        running.control.wait().await;
    };
    if tokio::time::timeout(Duration::from_secs(3), polite)
        .await
        .is_err()
    {
        running.control.kill();
    }
}

fn client_capabilities() -> lsp_types::ClientCapabilities {
    use lsp_types::*;
    ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![
                PositionEncodingKind::UTF8,
                PositionEncodingKind::UTF16,
            ]),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities {
                did_save: Some(true),
                ..Default::default()
            }),
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(false),
                    resolve_support: Some(CompletionItemCapabilityResolveSupport {
                        properties: vec![
                            "additionalTextEdits".into(),
                            "detail".into(),
                            "documentation".into(),
                        ],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            hover: Some(HoverClientCapabilities {
                content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                ..Default::default()
            }),
            definition: Some(GotoCapability {
                link_support: Some(true),
                ..Default::default()
            }),
            code_action: Some(CodeActionClientCapabilities {
                code_action_literal_support: Some(CodeActionLiteralSupport {
                    code_action_kind: CodeActionKindLiteralSupport {
                        value_set: [
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::REFACTOR,
                            CodeActionKind::SOURCE,
                            CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
                        ]
                        .iter()
                        .map(|k| k.as_str().to_owned())
                        .collect(),
                    },
                }),
                resolve_support: Some(CodeActionCapabilityResolveSupport {
                    properties: vec!["edit".into()],
                }),
                data_support: Some(true),
                is_preferred_support: Some(true),
                disabled_support: Some(true),
                ..Default::default()
            }),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities::default()),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            apply_edit: Some(true),
            workspace_edit: Some(WorkspaceEditClientCapabilities {
                document_changes: Some(true),
                ..Default::default()
            }),
            configuration: Some(true),
            workspace_folders: Some(true),
            ..Default::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Messages a server starts.
struct Handler {
    shared: Weak<Shared>,
    key: ServerKey,
    encoding: Mutex<Encoding>,
}

impl Handler {
    fn default_notification(&self, method: &str, params: Value) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        match method {
            "textDocument/publishDiagnostics" => {
                let Ok(params) =
                    serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params)
                else {
                    return;
                };
                let Some(uri) = convert::uri_from_lsp(&params.uri) else {
                    return;
                };
                let encoding = *lock(&self.encoding);
                let key = self.key.clone();
                let open = lock(&shared.state).docs.get(&uri).map(|d| d.rope.clone());
                let weak = self.shared.clone();
                tokio::spawn(async move {
                    let Some(shared) = weak.upgrade() else {
                        return;
                    };
                    let rope = match open {
                        Some(rope) => rope,
                        None => shared
                            .host
                            .read_text(&uri)
                            .await
                            .map(|t| Rope::from_str(&t))
                            .unwrap_or_default(),
                    };
                    let list = params
                        .diagnostics
                        .into_iter()
                        .map(|raw| (convert::diagnostic(&rope, encoding, &key.id, &raw), raw))
                        .collect();
                    shared.set_diagnostics(&key, uri, list);
                });
            }
            "$/progress" => {
                let token = match &params["token"] {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let value = &params["value"];
                let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
                let percentage = value
                    .get("percentage")
                    .and_then(Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok());
                {
                    let mut state = lock(&shared.state);
                    let entry = state
                        .progress
                        .iter()
                        .position(|(k, t, _)| *k == self.key && *t == token);
                    match (value.get("kind").and_then(Value::as_str), entry) {
                        (Some("begin"), _) => {
                            if let Some(i) = entry {
                                state.progress.remove(i);
                            }
                            state.progress.push((
                                self.key.clone(),
                                token,
                                Progress {
                                    server: self.key.id.clone(),
                                    title: text("title").unwrap_or_default(),
                                    message: text("message"),
                                    percentage,
                                },
                            ));
                        }
                        (Some("report"), Some(i)) => {
                            let progress = &mut state.progress[i].2;
                            if let Some(message) = text("message") {
                                progress.message = Some(message);
                            }
                            progress.percentage = percentage.or(progress.percentage);
                        }
                        (Some("end"), Some(i)) => {
                            state.progress.remove(i);
                        }
                        _ => return,
                    }
                }
                shared.host.status_changed();
            }
            "window/logMessage" | "window/showMessage" => {
                let message = params.get("message").and_then(Value::as_str).unwrap_or("");
                shared.host.log(&self.key.id, message);
            }
            _ => tracing::trace!(server = %self.key.id, method, "notification ignored"),
        }
    }

    fn default_request(
        &self,
        method: &str,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, ResponseError>> {
        let shared = self.shared.clone();
        let encoding = *lock(&self.encoding);
        let method = method.to_owned();
        Box::pin(async move {
            let Some(shared) = shared.upgrade() else {
                return Err(ResponseError {
                    code: ResponseError::REQUEST_CANCELLED,
                    message: "the IDE is shutting down".into(),
                });
            };
            match method.as_str() {
                "workspace/applyEdit" => {
                    let params: lsp_types::ApplyWorkspaceEditParams =
                        serde_json::from_value(params).map_err(|e| ResponseError {
                            code: -32602,
                            message: e.to_string(),
                        })?;
                    let edit = shared.workspace_edit(encoding, &params.edit).await;
                    let applied = shared.host.apply_edit(edit).await;
                    Ok(json!({ "applied": applied }))
                }
                // Settings come with initializationOptions in the MVP; one null per item.
                "workspace/configuration" => {
                    let count = params
                        .get("items")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    Ok(Value::Array(vec![Value::Null; count]))
                }
                "workspace/workspaceFolders" => {
                    let folders: Vec<Value> = shared
                        .host
                        .workspace_folders()
                        .iter()
                        .map(|f| json!({"uri": f.as_str(), "name": f.as_str()}))
                        .collect();
                    Ok(Value::Array(folders))
                }
                "window/workDoneProgress/create"
                | "client/registerCapability"
                | "client/unregisterCapability"
                | "window/showMessageRequest" => Ok(Value::Null),
                other => Err(ResponseError::method_not_found(other)),
            }
        })
    }
}

impl Handler {
    /// The server's declaration and the methods its plugin intercepts.
    fn middleware(&self, method: &str) -> Option<(Arc<Shared>, ServerDecl)> {
        let shared = self.shared.upgrade()?;
        let decl = lock(&shared.state)
            .servers
            .get(&self.key)
            .map(|s| s.decl.clone())?;
        shared
            .host
            .middleware(&decl)
            .iter()
            .any(|m| m == method)
            .then_some((shared, decl))
    }
}

impl ServerHandler for Handler {
    fn notification(&self, method: &str, params: Value) {
        let Some((shared, decl)) = self.middleware(method) else {
            self.default_notification(method, params);
            return;
        };
        // The plugin sees it first; `forward` hands it on (maybe changed).
        let key = self.key.clone();
        let encoding = *lock(&self.encoding);
        let method = method.to_owned();
        let weak = self.shared.clone();
        tokio::spawn(async move {
            match shared.host.intercept(&decl, &method, params).await {
                Ok(MiddlewareResult::Forward { params }) => {
                    let handler = Handler {
                        shared: weak,
                        key,
                        encoding: Mutex::new(encoding),
                    };
                    handler.default_notification(&method, params);
                }
                Ok(MiddlewareResult::Respond { .. }) => {}
                Err(error) => shared.host.log(&decl.id, &format!("{method}: {error}")),
            }
        });
    }

    fn request(
        &self,
        method: &str,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, ResponseError>> {
        let Some((shared, decl)) = self.middleware(method) else {
            return self.default_request(method, params);
        };
        let handler = Handler {
            shared: self.shared.clone(),
            key: self.key.clone(),
            encoding: Mutex::new(*lock(&self.encoding)),
        };
        let method = method.to_owned();
        Box::pin(async move {
            match shared.host.intercept(&decl, &method, params).await {
                Ok(MiddlewareResult::Respond { result }) => Ok(result),
                Ok(MiddlewareResult::Forward { params }) => {
                    handler.default_request(&method, params).await
                }
                Err(message) => Err(ResponseError {
                    code: -32603,
                    message,
                }),
            }
        })
    }
}
