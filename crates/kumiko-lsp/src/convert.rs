//! Server results into the service's types. Every range goes through the text the positions
//! refer to (see [`crate::encoding`]).

use ropey::Rope;

use kumiko_types::{TextEdit, Uri};

use crate::encoding::{Encoding, range_from_lsp};
use crate::types::{CompletionItem, CompletionKind, Diagnostic, Hover, Severity};

pub fn uri_to_lsp(uri: &Uri) -> lsp_types::Uri {
    // Both are RFC 3986 URIs; ours are always valid, so the parse does not fail in practice.
    uri.as_str()
        .parse()
        .unwrap_or_else(|_| "file:///".parse().unwrap_or_else(|_| unreachable!()))
}

pub fn uri_from_lsp(uri: &lsp_types::Uri) -> Option<Uri> {
    Uri::parse(uri.as_str()).ok()
}

pub fn text_edit(rope: &Rope, encoding: Encoding, edit: &lsp_types::TextEdit) -> TextEdit {
    TextEdit {
        range: range_from_lsp(rope, encoding, edit.range),
        new_text: edit.new_text.clone(),
    }
}

pub fn diagnostic(
    rope: &Rope,
    encoding: Encoding,
    server: &str,
    raw: &lsp_types::Diagnostic,
) -> Diagnostic {
    let severity = match raw.severity {
        Some(lsp_types::DiagnosticSeverity::WARNING) => Severity::Warning,
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => Severity::Information,
        Some(lsp_types::DiagnosticSeverity::HINT) => Severity::Hint,
        _ => Severity::Error,
    };
    Diagnostic {
        range: range_from_lsp(rope, encoding, raw.range),
        severity,
        message: raw.message.clone(),
        source: raw.source.clone().unwrap_or_else(|| server.to_owned()),
        code: raw.code.as_ref().map(|code| match code {
            lsp_types::NumberOrString::Number(n) => n.to_string(),
            lsp_types::NumberOrString::String(s) => s.clone(),
        }),
    }
}

fn completion_kind(kind: Option<lsp_types::CompletionItemKind>) -> CompletionKind {
    use lsp_types::CompletionItemKind as K;
    match kind {
        Some(K::TEXT) => CompletionKind::Text,
        Some(K::METHOD) => CompletionKind::Method,
        Some(K::FUNCTION) => CompletionKind::Function,
        Some(K::CONSTRUCTOR) => CompletionKind::Constructor,
        Some(K::FIELD) => CompletionKind::Field,
        Some(K::VARIABLE) => CompletionKind::Variable,
        Some(K::CLASS) => CompletionKind::Class,
        Some(K::INTERFACE) => CompletionKind::Interface,
        Some(K::MODULE) => CompletionKind::Module,
        Some(K::PROPERTY) => CompletionKind::Property,
        Some(K::KEYWORD) => CompletionKind::Keyword,
        Some(K::SNIPPET) => CompletionKind::Snippet,
        Some(K::CONSTANT) => CompletionKind::Constant,
        Some(K::ENUM) => CompletionKind::Enum,
        _ => CompletionKind::Other,
    }
}

pub fn completion_item(
    rope: &Rope,
    encoding: Encoding,
    raw: &lsp_types::CompletionItem,
) -> CompletionItem {
    let edit = raw.text_edit.as_ref().map(|edit| match edit {
        lsp_types::CompletionTextEdit::Edit(edit) => text_edit(rope, encoding, edit),
        // Replacing is what IntelliJ does on Enter for an identifier under the caret too;
        // keep the plain insert range (Tab-to-replace may come later).
        lsp_types::CompletionTextEdit::InsertAndReplace(edit) => TextEdit {
            range: range_from_lsp(rope, encoding, edit.insert),
            new_text: edit.new_text.clone(),
        },
    });
    CompletionItem {
        label: raw.label.clone(),
        kind: completion_kind(raw.kind),
        detail: raw.detail.clone(),
        insert_text: raw.insert_text.clone().unwrap_or_else(|| raw.label.clone()),
        edit,
        additional_edits: raw
            .additional_text_edits
            .iter()
            .flatten()
            .map(|e| text_edit(rope, encoding, e))
            .collect(),
        sort_text: raw.sort_text.clone().unwrap_or_else(|| raw.label.clone()),
        filter_text: raw.filter_text.clone().unwrap_or_else(|| raw.label.clone()),
        data: None,
    }
}

pub fn hover(rope: &Rope, encoding: Encoding, raw: lsp_types::Hover) -> Hover {
    fn marked(string: lsp_types::MarkedString) -> String {
        match string {
            lsp_types::MarkedString::String(s) => s,
            lsp_types::MarkedString::LanguageString(code) => {
                format!("```{}\n{}\n```", code.language, code.value)
            }
        }
    }
    let contents = match raw.contents {
        lsp_types::HoverContents::Scalar(string) => marked(string),
        lsp_types::HoverContents::Array(strings) => strings
            .into_iter()
            .map(marked)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lsp_types::HoverContents::Markup(markup) => markup.value,
    };
    Hover {
        contents,
        range: raw.range.map(|r| range_from_lsp(rope, encoding, r)),
    }
}

/// Text edits of an LSP workspace edit per file, still in the server's positions. Resource
/// operations (create, rename, delete) are not supported yet and are reported.
pub fn workspace_edit_parts(
    edit: &lsp_types::WorkspaceEdit,
) -> (Vec<(lsp_types::Uri, Vec<lsp_types::TextEdit>)>, Vec<String>) {
    let mut parts = Vec::new();
    let mut unsupported = Vec::new();
    if let Some(changes) = &edit.document_changes {
        let mut document = |edit: &lsp_types::TextDocumentEdit| {
            let edits = edit
                .edits
                .iter()
                .map(|e| match e {
                    lsp_types::OneOf::Left(edit) => edit.clone(),
                    lsp_types::OneOf::Right(annotated) => annotated.text_edit.clone(),
                })
                .collect();
            parts.push((edit.text_document.uri.clone(), edits));
        };
        match changes {
            lsp_types::DocumentChanges::Edits(edits) => edits.iter().for_each(&mut document),
            lsp_types::DocumentChanges::Operations(operations) => {
                for operation in operations {
                    match operation {
                        lsp_types::DocumentChangeOperation::Edit(edit) => document(edit),
                        lsp_types::DocumentChangeOperation::Op(op) => {
                            unsupported.push(format!("{op:?}"));
                        }
                    }
                }
            }
        }
    } else if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            parts.push((uri.clone(), edits.clone()));
        }
        // HashMap order: sort for stable application and tests.
        parts.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    }
    (parts, unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_items_take_the_servers_edit() {
        let rope = Rope::from_str("x.пр\n");
        let raw: lsp_types::CompletionItem = serde_json::from_value(serde_json::json!({
            "label": "println",
            "kind": 2,
            "textEdit": {"range": {"start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 4}}, "newText": "println()"},
            "additionalTextEdits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "import a;\n"}]
        }))
        .unwrap();
        let item = completion_item(&rope, Encoding::Utf16, &raw);
        assert_eq!(item.kind, CompletionKind::Method);
        let edit = item.edit.unwrap();
        assert_eq!(
            (edit.range.start.character, edit.range.end.character),
            (2, 6)
        );
        assert_eq!(item.additional_edits.len(), 1);
        assert_eq!(item.sort_text, "println");
    }

    #[test]
    fn workspace_edits_come_from_either_form() {
        let edit: lsp_types::WorkspaceEdit = serde_json::from_value(serde_json::json!({
            "documentChanges": [
                {"textDocument": {"uri": "file:///a", "version": 1}, "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "x"}]},
                {"kind": "create", "uri": "file:///b"}
            ]
        }))
        .unwrap();
        let (parts, unsupported) = workspace_edit_parts(&edit);
        assert_eq!(parts.len(), 1);
        assert_eq!(unsupported.len(), 1);
        let edit: lsp_types::WorkspaceEdit = serde_json::from_value(serde_json::json!({
            "changes": {"file:///b": [], "file:///a": []}
        }))
        .unwrap();
        let (parts, _) = workspace_edit_parts(&edit);
        assert_eq!(parts[0].0.as_str(), "file:///a");
    }
}
