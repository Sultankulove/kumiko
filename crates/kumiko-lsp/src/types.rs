//! What the editor and the UI get from the Language service: plain data in the IDE's
//! positions (UTF-8), independent of LSP's types.

use serde_json::Value;

use kumiko_types::{Range, TextEdit, Uri};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Severity,
    pub message: String,
    /// Who reported it, e.g. the server's `source` or its id.
    pub source: String,
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Location {
    pub uri: Uri,
    pub range: Range,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Keyword,
    Snippet,
    Constant,
    Enum,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
    /// What to insert: `edit` when the server gave a range, otherwise `insert_text` at the
    /// word around the caret.
    pub insert_text: String,
    pub edit: Option<TextEdit>,
    /// Imports and the like, applied together with the item.
    pub additional_edits: Vec<TextEdit>,
    pub sort_text: String,
    pub filter_text: String,
    /// The server's item, kept for `completionItem/resolve`.
    pub(crate) data: Option<(crate::ServerKey, Value)>,
}

impl CompletionItem {
    /// An item that inserts its label (for tests and providers without a server).
    pub fn plain(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            kind: CompletionKind::Text,
            detail: None,
            insert_text: label.to_owned(),
            edit: None,
            additional_edits: Vec::new(),
            sort_text: label.to_owned(),
            filter_text: label.to_owned(),
            data: None,
        }
    }
}

/// Edits across files, already in the IDE's positions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkspaceEdit {
    pub changes: Vec<(Uri, Vec<TextEdit>)>,
}

impl WorkspaceEdit {
    pub fn is_empty(&self) -> bool {
        self.changes.iter().all(|(_, edits)| edits.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub preferred: bool,
    pub(crate) server: crate::ServerKey,
    /// The server's action or command, for resolving and running it.
    pub(crate) raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hover {
    /// Markdown or plain text.
    pub contents: String,
    pub range: Option<Range>,
}
