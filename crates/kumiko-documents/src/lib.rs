//! Documents service: rope buffers, versions, transactions, undo/redo (spec, section 12;
//! ADR-004).
//!
//! The core owns every open text; editors only render snapshots and send transactions.
//! A transaction is a set of non-overlapping edits against `base_version`; each one bumps
//! the version by one. Edits against an older version are rebased through the recorded
//! history, a conflict fails with [`DocError::ContentModified`]. Undo history lives here, so
//! it survives replacing a view. Every change is published as `document.changed`.

mod encoding;
mod text;

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ropey::Rope;

use kumiko_protocol::events::{DocumentChanged, DocumentClosed, DocumentOpened, DocumentSaved};
use kumiko_service_api::{Event, EventSink, FileSystem, FsError, WriteOptions};
use kumiko_types::{DocumentId, DocumentVersion, Position, Range, TextEdit, Uri};

pub use encoding::{Encoding, LineEnding, TextFormat};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DocError {
    #[error("document {0} is not open")]
    NotOpen(DocumentId),
    #[error(transparent)]
    Fs(#[from] FsError),
    #[error("position {}:{} is outside the text or inside a character", .0.line, .0.character)]
    InvalidPosition(Position),
    #[error("range ends before it starts")]
    InvalidRange(Range),
    #[error("edits of one transaction overlap")]
    OverlappingEdits,
    #[error("base version {requested:?} is newer than the document ({current:?})")]
    VersionAhead {
        current: DocumentVersion,
        requested: DocumentVersion,
    },
    #[error("the document changed since the edits were computed")]
    ContentModified,
    #[error("{0} is read-only")]
    ReadOnly(Uri),
    #[error("{uri} cannot be saved in its encoding: `{character}` is not representable")]
    Unencodable { uri: Uri, character: char },
}

#[derive(Debug, Clone)]
pub struct DocumentsConfig {
    /// Files above this size are marked large: no syntax trees or language servers.
    pub large_file_bytes: u64,
    /// Files above this size open read-only.
    pub read_only_bytes: u64,
    /// Transactions kept for rebasing stale edits.
    pub history_len: usize,
    /// Edits closer in time than this undo together.
    pub undo_group_window: Duration,
}

impl Default for DocumentsConfig {
    fn default() -> Self {
        Self {
            large_file_bytes: 20 * 1024 * 1024,
            read_only_bytes: 200 * 1024 * 1024,
            history_len: 1000,
            undo_group_window: Duration::from_millis(500),
        }
    }
}

/// Immutable view of a document at one version; cloning the text is cheap.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: DocumentId,
    pub uri: Uri,
    pub version: DocumentVersion,
    pub language_id: Option<String>,
    pub text: Rope,
    /// Differs from the stored file.
    pub dirty: bool,
    pub read_only: bool,
    pub large: bool,
    /// The file changed or disappeared on disk while the document had unsaved edits.
    pub conflict: bool,
    pub format: TextFormat,
}

/// What an external change of a file did to its open document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalChange {
    NotOpen,
    /// The file has the content the document last read or wrote.
    Unchanged,
    /// The clean document was reloaded.
    Reloaded(DocumentVersion),
    /// The document has unsaved edits; the user must choose.
    Conflict,
    /// The file is gone.
    Deleted,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Edit,
    Undo,
    Redo,
    Reload,
}

struct Document {
    id: DocumentId,
    uri: Uri,
    language_id: Option<String>,
    text: Rope,
    version: DocumentVersion,
    saved_version: Option<DocumentVersion>,
    /// The text as stored, to see that edits or undo brought it back (the rope shares its
    /// nodes with `text`, so keeping it is cheap).
    saved_text: Rope,
    format: TextFormat,
    disk_hash: Option<u64>,
    pending_hash: Option<u64>,
    read_only: bool,
    large: bool,
    conflict: bool,
    /// `(version after, edits in application order)`, oldest first.
    history: VecDeque<(DocumentVersion, Vec<TextEdit>)>,
    undo: Vec<Vec<Vec<TextEdit>>>,
    redo: Vec<Vec<Vec<TextEdit>>>,
    last_edit: Option<Instant>,
}

impl Document {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            id: self.id,
            uri: self.uri.clone(),
            version: self.version,
            language_id: self.language_id.clone(),
            text: self.text.clone(),
            dirty: self.saved_version != Some(self.version),
            read_only: self.read_only,
            large: self.large,
            conflict: self.conflict,
            format: self.format,
        }
    }

    fn end(&self) -> Position {
        let last = self.text.len_lines() - 1;
        Position::new(last as u32, self.text.line(last).len_bytes() as u32)
    }
}

#[derive(Default)]
struct State {
    docs: HashMap<DocumentId, Document>,
    by_uri: HashMap<Uri, DocumentId>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

pub struct DocumentService {
    fs: Arc<dyn FileSystem>,
    events: Arc<dyn EventSink>,
    config: DocumentsConfig,
    state: Mutex<State>,
    next_id: AtomicU64,
}

impl DocumentService {
    pub fn new(
        fs: Arc<dyn FileSystem>,
        events: Arc<dyn EventSink>,
        config: DocumentsConfig,
    ) -> Self {
        Self {
            fs,
            events,
            config,
            state: Mutex::default(),
            next_id: AtomicU64::new(1),
        }
    }

    /// Opens a stored document, or returns the already open one.
    pub async fn open(&self, uri: &Uri, language_id: Option<String>) -> Result<Snapshot, DocError> {
        if let Some(snapshot) = self.find(uri).and_then(|id| self.snapshot(id).ok()) {
            return Ok(snapshot);
        }
        let bytes = self.fs.read(uri).await?;
        let size = bytes.len() as u64;
        let disk_hash = hash(&bytes);
        let (text, format) = encoding::decode(&bytes);

        let mut state = lock(&self.state);
        if let Some(&id) = state.by_uri.get(uri) {
            return Ok(state.docs[&id].snapshot());
        }
        let id = DocumentId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let version = DocumentVersion(1);
        let document = Document {
            id,
            uri: uri.clone(),
            language_id,
            text: Rope::from_str(&text),
            version,
            saved_version: Some(version),
            saved_text: Rope::from_str(&text),
            format,
            disk_hash: Some(disk_hash),
            pending_hash: None,
            read_only: size > self.config.read_only_bytes,
            large: size > self.config.large_file_bytes,
            conflict: false,
            history: VecDeque::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
        };
        let snapshot = document.snapshot();
        state.by_uri.insert(uri.clone(), id);
        state.docs.insert(id, document);
        self.events.publish(Event::core(&DocumentOpened {
            uri: uri.clone(),
            version,
            language_id: snapshot.language_id.clone(),
        }));
        Ok(snapshot)
    }

    pub fn find(&self, uri: &Uri) -> Option<DocumentId> {
        lock(&self.state).by_uri.get(uri).copied()
    }

    pub fn snapshot(&self, id: DocumentId) -> Result<Snapshot, DocError> {
        let state = lock(&self.state);
        state
            .docs
            .get(&id)
            .map(Document::snapshot)
            .ok_or(DocError::NotOpen(id))
    }

    /// Open documents, oldest first.
    pub fn list(&self) -> Vec<Snapshot> {
        let state = lock(&self.state);
        let mut list: Vec<_> = state.docs.values().map(Document::snapshot).collect();
        list.sort_by_key(|s| s.id);
        list
    }

    pub fn set_language(
        &self,
        id: DocumentId,
        language_id: Option<String>,
    ) -> Result<(), DocError> {
        let mut state = lock(&self.state);
        let doc = state.docs.get_mut(&id).ok_or(DocError::NotOpen(id))?;
        doc.language_id = language_id;
        Ok(())
    }

    /// The edits of every version after `version`, oldest first, each in the coordinates of
    /// the text before it; `None` when the history no longer reaches back that far.
    pub fn changes_since(
        &self,
        id: DocumentId,
        version: DocumentVersion,
    ) -> Option<Vec<Vec<TextEdit>>> {
        let state = lock(&self.state);
        let doc = state.docs.get(&id)?;
        if version >= doc.version {
            return Some(Vec::new());
        }
        let first_kept = doc.history.front().map(|(v, _)| v.0)?;
        if first_kept > version.0 + 1 {
            return None;
        }
        Some(
            doc.history
                .iter()
                .filter(|(v, _)| *v > version)
                .map(|(_, edits)| edits.clone())
                .collect(),
        )
    }

    /// Applies edits computed against `base_version` as one transaction.
    pub fn apply(
        &self,
        id: DocumentId,
        base_version: DocumentVersion,
        edits: Vec<TextEdit>,
    ) -> Result<DocumentVersion, DocError> {
        let mut state = lock(&self.state);
        let doc = state.docs.get_mut(&id).ok_or(DocError::NotOpen(id))?;
        if doc.read_only {
            return Err(DocError::ReadOnly(doc.uri.clone()));
        }
        if base_version > doc.version {
            return Err(DocError::VersionAhead {
                current: doc.version,
                requested: base_version,
            });
        }
        let edits = if base_version == doc.version {
            edits
        } else {
            let first_kept = doc.history.front().map(|(v, _)| v.0);
            if first_kept.is_none_or(|first| first > base_version.0 + 1) {
                return Err(DocError::ContentModified);
            }
            let mut rebased = Vec::with_capacity(edits.len());
            for edit in &edits {
                let mut current = edit.clone();
                for (_, later) in doc.history.iter().filter(|(v, _)| *v > base_version) {
                    current = text::rebase(&current, later).ok_or(DocError::ContentModified)?;
                }
                rebased.push(current);
            }
            rebased
        };
        self.commit(doc, edits, Origin::Edit)
    }

    /// Applies edits and records history, undo and the change event.
    fn commit(
        &self,
        doc: &mut Document,
        edits: Vec<TextEdit>,
        origin: Origin,
    ) -> Result<DocumentVersion, DocError> {
        let applied = text::apply(&mut doc.text, edits)?;
        doc.version = doc.version.next();
        // Back to the stored text (all edits undone, or typed back): clean again. The full
        // comparison runs only when the lengths match.
        if doc.text.len_bytes() == doc.saved_text.len_bytes() && doc.text == doc.saved_text {
            doc.saved_version = Some(doc.version);
        }
        doc.history.push_back((doc.version, applied.edits.clone()));
        while doc.history.len() > self.config.history_len {
            doc.history.pop_front();
        }

        let now = Instant::now();
        match origin {
            Origin::Edit => {
                let joins = doc
                    .last_edit
                    .is_some_and(|t| now.duration_since(t) < self.config.undo_group_window);
                match doc.undo.last_mut() {
                    Some(group) if joins => group.push(applied.inverse),
                    _ => doc.undo.push(vec![applied.inverse]),
                }
                doc.redo.clear();
                doc.last_edit = Some(now);
            }
            Origin::Reload => {
                doc.undo.push(vec![applied.inverse]);
                doc.redo.clear();
                doc.last_edit = None;
            }
            Origin::Undo => {
                if let Some(group) = doc.redo.last_mut() {
                    group.push(applied.inverse);
                }
                doc.last_edit = None;
            }
            Origin::Redo => {
                if let Some(group) = doc.undo.last_mut() {
                    group.push(applied.inverse);
                }
                doc.last_edit = None;
            }
        }

        self.events.publish(Event::core(&DocumentChanged {
            uri: doc.uri.clone(),
            version: doc.version,
            changes: applied.edits,
        }));
        Ok(doc.version)
    }

    /// Undoes the last group of edits; `None` when there is nothing to undo.
    pub fn undo(&self, id: DocumentId) -> Result<Option<DocumentVersion>, DocError> {
        self.replay(id, Origin::Undo)
    }

    /// Redoes the last undone group; `None` when there is nothing to redo.
    pub fn redo(&self, id: DocumentId) -> Result<Option<DocumentVersion>, DocError> {
        self.replay(id, Origin::Redo)
    }

    fn replay(&self, id: DocumentId, origin: Origin) -> Result<Option<DocumentVersion>, DocError> {
        let mut state = lock(&self.state);
        let doc = state.docs.get_mut(&id).ok_or(DocError::NotOpen(id))?;
        if doc.read_only {
            return Err(DocError::ReadOnly(doc.uri.clone()));
        }
        let (from, to) = match origin {
            Origin::Undo => (&mut doc.undo, &mut doc.redo),
            _ => (&mut doc.redo, &mut doc.undo),
        };
        let Some(group) = from.pop() else {
            return Ok(None);
        };
        to.push(Vec::with_capacity(group.len()));
        let mut version = None;
        for inverse in group.into_iter().rev() {
            version = Some(self.commit(doc, inverse, origin)?);
        }
        Ok(version)
    }

    /// Writes the document in its original encoding and line endings.
    pub async fn save(&self, id: DocumentId) -> Result<DocumentVersion, DocError> {
        let (uri, bytes, version, content_hash, text) = {
            let mut state = lock(&self.state);
            let doc = state.docs.get_mut(&id).ok_or(DocError::NotOpen(id))?;
            let bytes =
                encoding::encode(&doc.text.to_string(), doc.format).map_err(|character| {
                    DocError::Unencodable {
                        uri: doc.uri.clone(),
                        character,
                    }
                })?;
            let content_hash = hash(&bytes);
            doc.pending_hash = Some(content_hash);
            (
                doc.uri.clone(),
                bytes,
                doc.version,
                content_hash,
                doc.text.clone(),
            )
        };
        let written = self.fs.write(&uri, bytes, WriteOptions::UPSERT).await;

        let mut state = lock(&self.state);
        let Some(doc) = state.docs.get_mut(&id) else {
            written?;
            return Ok(version);
        };
        doc.pending_hash = None;
        written?;
        doc.disk_hash = Some(content_hash);
        doc.saved_version = Some(version);
        doc.saved_text = text;
        if doc.version != version && doc.text == doc.saved_text {
            doc.saved_version = Some(doc.version);
        }
        doc.conflict = false;
        self.events
            .publish(Event::core(&DocumentSaved { uri, version }));
        Ok(version)
    }

    /// Replaces the text with the stored file, discarding unsaved edits.
    pub async fn revert(&self, id: DocumentId) -> Result<DocumentVersion, DocError> {
        let uri = self.snapshot(id)?.uri;
        let bytes = self.fs.read(&uri).await?;
        let mut state = lock(&self.state);
        let doc = state.docs.get_mut(&id).ok_or(DocError::NotOpen(id))?;
        self.reload(doc, &bytes)
    }

    fn reload(&self, doc: &mut Document, bytes: &[u8]) -> Result<DocumentVersion, DocError> {
        let (text, format) = encoding::decode(bytes);
        let whole = TextEdit {
            range: Range::new(Position::new(0, 0), doc.end()),
            new_text: text,
        };
        let version = self.commit(doc, vec![whole], Origin::Reload)?;
        doc.format = format;
        doc.saved_version = Some(version);
        doc.saved_text = doc.text.clone();
        doc.disk_hash = Some(hash(bytes));
        doc.conflict = false;
        Ok(version)
    }

    /// Reacts to a change of the stored file: a clean document is reloaded silently, a dirty
    /// one is marked as conflicting.
    pub async fn external_change(&self, uri: &Uri) -> Result<ExternalChange, DocError> {
        let Some(id) = self.find(uri) else {
            return Ok(ExternalChange::NotOpen);
        };
        let read = self.fs.read(uri).await;
        let mut state = lock(&self.state);
        let Some(doc) = state.docs.get_mut(&id) else {
            return Ok(ExternalChange::NotOpen);
        };
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(FsError::NotFound(_)) => {
                doc.conflict = true;
                doc.disk_hash = None;
                return Ok(ExternalChange::Deleted);
            }
            Err(error) => return Err(error.into()),
        };
        let content_hash = hash(&bytes);
        if Some(content_hash) == doc.disk_hash || Some(content_hash) == doc.pending_hash {
            return Ok(ExternalChange::Unchanged);
        }
        if doc.saved_version != Some(doc.version) {
            doc.conflict = true;
            return Ok(ExternalChange::Conflict);
        }
        let version = self.reload(doc, &bytes)?;
        tracing::debug!(%uri, ?version, "reloaded a document changed on disk");
        Ok(ExternalChange::Reloaded(version))
    }

    /// Keeps the buffer after a conflict; the next save overwrites the file.
    pub fn keep_mine(&self, id: DocumentId) -> Result<(), DocError> {
        let mut state = lock(&self.state);
        let doc = state.docs.get_mut(&id).ok_or(DocError::NotOpen(id))?;
        doc.conflict = false;
        Ok(())
    }

    pub fn close(&self, id: DocumentId) -> Result<(), DocError> {
        let mut state = lock(&self.state);
        let doc = state.docs.remove(&id).ok_or(DocError::NotOpen(id))?;
        state.by_uri.remove(&doc.uri);
        self.events
            .publish(Event::core(&DocumentClosed { uri: doc.uri }));
        Ok(())
    }
}

#[cfg(test)]
mod tests;
