//! Navigation and refactoring features: Find Usages, rename, reformat, file structure,
//! highlighting of usages, parameter info.

use kumiko_types::{Position, TextEdit, Uri};

use super::{LanguageService, position_params};
use crate::convert;
use crate::encoding::range_from_lsp;
use crate::types::{Highlight, Location, SignatureHelp, Symbol, WorkspaceEdit};

/// A capability given as `true` or as options.
fn enabled<T>(capability: &Option<lsp_types::OneOf<bool, T>>) -> bool {
    matches!(
        capability,
        Some(lsp_types::OneOf::Left(true) | lsp_types::OneOf::Right(_))
    )
}

fn kind_name(kind: lsp_types::SymbolKind) -> &'static str {
    use lsp_types::SymbolKind as K;
    match kind {
        K::FILE => "file",
        K::MODULE => "module",
        K::NAMESPACE => "namespace",
        K::PACKAGE => "package",
        K::CLASS => "class",
        K::METHOD => "method",
        K::PROPERTY => "property",
        K::FIELD => "field",
        K::CONSTRUCTOR => "constructor",
        K::ENUM => "enum",
        K::INTERFACE => "interface",
        K::FUNCTION => "function",
        K::VARIABLE => "variable",
        K::CONSTANT => "constant",
        K::ENUM_MEMBER => "enumMember",
        K::STRUCT => "struct",
        K::TYPE_PARAMETER => "typeParameter",
        _ => "symbol",
    }
}

impl LanguageService {
    /// Find Usages: every reference, the declaration included, of all servers.
    pub async fn references(&self, uri: &Uri, position: Position) -> Vec<Location> {
        let servers = self
            .shared
            .ready(uri, |c| enabled(&c.references_provider))
            .await;
        let mut found = Vec::new();
        for (key, server) in servers {
            let Some(params) =
                self.shared
                    .with_doc(uri, &server, |doc| lsp_types::ReferenceParams {
                        text_document_position: position_params(
                            uri,
                            doc,
                            server.encoding,
                            position,
                        ),
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                        context: lsp_types::ReferenceContext {
                            include_declaration: true,
                        },
                    })
            else {
                continue;
            };
            let locations = match server
                .client
                .request::<lsp_types::request::References>(params)
                .await
            {
                Ok(locations) => locations.unwrap_or_default(),
                Err(error) => {
                    self.shared
                        .host
                        .log(&key.id, &format!("references: {error}"));
                    continue;
                }
            };
            for location in locations {
                let Some(target) = convert::uri_from_lsp(&location.uri) else {
                    continue;
                };
                let range = self
                    .shared
                    .convert_range(&target, server.encoding, location.range)
                    .await;
                let location = Location { uri: target, range };
                if !found.contains(&location) {
                    found.push(location);
                }
            }
        }
        found.sort_by(|a, b| (a.uri.as_str(), a.range.start).cmp(&(b.uri.as_str(), b.range.start)));
        found
    }

    /// Renames the symbol everywhere and applies the edit; the number of files changed.
    pub async fn rename(
        &self,
        uri: &Uri,
        position: Position,
        new_name: &str,
    ) -> Result<usize, String> {
        let servers = self
            .shared
            .ready(uri, |c| enabled(&c.rename_provider))
            .await;
        let Some((_, server)) = servers.into_iter().next() else {
            return Err("no language server can rename here".into());
        };
        let params = self
            .shared
            .with_doc(uri, &server, |doc| lsp_types::RenameParams {
                text_document_position: position_params(uri, doc, server.encoding, position),
                new_name: new_name.to_owned(),
                work_done_progress_params: Default::default(),
            })
            .ok_or("the document is not open")?;
        let edit = server
            .client
            .request::<lsp_types::request::Rename>(params)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("nothing to rename here")?;
        let edit = self.shared.workspace_edit(server.encoding, &edit).await;
        let files = edit.changes.iter().filter(|(_, e)| !e.is_empty()).count();
        if self.shared.host.apply_edit(edit).await {
            Ok(files)
        } else {
            Err("the rename could not be applied".into())
        }
    }

    /// Reformat Code: the server's formatting edits, applied to the document.
    pub async fn format(
        &self,
        uri: &Uri,
        tab_size: u32,
        insert_spaces: bool,
    ) -> Result<(), String> {
        let servers = self
            .shared
            .ready(uri, |c| enabled(&c.document_formatting_provider))
            .await;
        let Some((_, server)) = servers.into_iter().next() else {
            return Err("no language server formats this file".into());
        };
        let version = self.shared.with_doc(uri, &server, |doc| doc.version);
        let params = lsp_types::DocumentFormattingParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: convert::uri_to_lsp(uri),
            },
            options: lsp_types::FormattingOptions {
                tab_size,
                insert_spaces,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        };
        let edits = server
            .client
            .request::<lsp_types::request::Formatting>(params)
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        if edits.is_empty() {
            return Ok(());
        }
        let edits: Vec<TextEdit> = self
            .shared
            .with_doc(uri, &server, |doc| {
                // Edits for an older text would land in the wrong places.
                if Some(doc.version) != version {
                    return None;
                }
                Some(
                    edits
                        .iter()
                        .map(|e| convert::text_edit(&doc.rope, server.encoding, e))
                        .collect(),
                )
            })
            .flatten()
            .ok_or("the document changed while it was formatted")?;
        let edit = WorkspaceEdit {
            changes: vec![(uri.clone(), edits)],
        };
        if self.shared.host.apply_edit(edit).await {
            Ok(())
        } else {
            Err("the formatting could not be applied".into())
        }
    }

    /// File Structure: the document's symbols in order, nested ones after their parent.
    pub async fn symbols(&self, uri: &Uri) -> Vec<Symbol> {
        let servers = self
            .shared
            .ready(uri, |c| enabled(&c.document_symbol_provider))
            .await;
        for (key, server) in servers {
            let params = lsp_types::DocumentSymbolParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: convert::uri_to_lsp(uri),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            };
            let response = match server
                .client
                .request::<lsp_types::request::DocumentSymbolRequest>(params)
                .await
            {
                Ok(Some(response)) => response,
                Ok(None) => continue,
                Err(error) => {
                    self.shared.host.log(&key.id, &format!("symbols: {error}"));
                    continue;
                }
            };
            let symbols = self.shared.with_doc(uri, &server, |doc| {
                let range = |r| range_from_lsp(&doc.rope, server.encoding, r);
                let mut out = Vec::new();
                match response {
                    lsp_types::DocumentSymbolResponse::Flat(list) => {
                        for s in list {
                            out.push(Symbol {
                                name: s.name,
                                kind: kind_name(s.kind),
                                detail: s.container_name,
                                range: range(s.location.range),
                                selection: range(s.location.range),
                                depth: 0,
                            });
                        }
                    }
                    lsp_types::DocumentSymbolResponse::Nested(list) => {
                        fn walk(
                            list: Vec<lsp_types::DocumentSymbol>,
                            depth: usize,
                            range: &dyn Fn(lsp_types::Range) -> kumiko_types::Range,
                            out: &mut Vec<Symbol>,
                        ) {
                            for s in list {
                                out.push(Symbol {
                                    name: s.name,
                                    kind: kind_name(s.kind),
                                    detail: s.detail,
                                    range: range(s.range),
                                    selection: range(s.selection_range),
                                    depth,
                                });
                                if let Some(children) = s.children {
                                    walk(children, depth + 1, range, out);
                                }
                            }
                        }
                        walk(list, 0, &range, &mut out);
                    }
                }
                out
            });
            if let Some(symbols) = symbols {
                return symbols;
            }
        }
        Vec::new()
    }

    /// Usages of the symbol under the caret in this document.
    pub async fn document_highlights(&self, uri: &Uri, position: Position) -> Vec<Highlight> {
        let servers = self
            .shared
            .ready(uri, |c| enabled(&c.document_highlight_provider))
            .await;
        for (_, server) in servers {
            let Some(params) =
                self.shared
                    .with_doc(uri, &server, |doc| lsp_types::DocumentHighlightParams {
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
            let Ok(Some(list)) = server
                .client
                .request::<lsp_types::request::DocumentHighlightRequest>(params)
                .await
            else {
                continue;
            };
            if let Some(highlights) = self.shared.with_doc(uri, &server, |doc| {
                list.into_iter()
                    .map(|h| Highlight {
                        range: range_from_lsp(&doc.rope, server.encoding, h.range),
                        write: h.kind == Some(lsp_types::DocumentHighlightKind::WRITE),
                    })
                    .collect()
            }) {
                return highlights;
            }
        }
        Vec::new()
    }

    /// Parameter Info: the signatures of the call around the caret.
    pub async fn signature_help(&self, uri: &Uri, position: Position) -> Option<SignatureHelp> {
        let servers = self
            .shared
            .ready(uri, |c| c.signature_help_provider.is_some())
            .await;
        for (_, server) in servers {
            let params =
                self.shared
                    .with_doc(uri, &server, |doc| lsp_types::SignatureHelpParams {
                        context: None,
                        text_document_position_params: position_params(
                            uri,
                            doc,
                            server.encoding,
                            position,
                        ),
                        work_done_progress_params: Default::default(),
                    })?;
            let Ok(Some(help)) = server
                .client
                .request::<lsp_types::request::SignatureHelpRequest>(params)
                .await
            else {
                continue;
            };
            if help.signatures.is_empty() {
                continue;
            }
            let active =
                (help.active_signature.unwrap_or(0) as usize).min(help.signatures.len() - 1);
            let mut signatures = help.signatures;
            let first = signatures.remove(active);
            let parameter_index = first
                .active_parameter
                .or(help.active_parameter)
                .unwrap_or(0) as usize;
            let active_parameter = first
                .parameters
                .as_ref()
                .and_then(|p| p.get(parameter_index))
                .and_then(|p| match &p.label {
                    lsp_types::ParameterLabel::Simple(text) => first
                        .label
                        .find(text.as_str())
                        .map(|start| start..start + text.len()),
                    // Offsets are UTF-16 units of the label.
                    lsp_types::ParameterLabel::LabelOffsets([from, to]) => {
                        let byte = |units: u32| {
                            let mut seen = 0;
                            first
                                .label
                                .char_indices()
                                .find(|(_, c)| {
                                    let before = seen;
                                    seen += c.len_utf16() as u32;
                                    before >= units
                                })
                                .map_or(first.label.len(), |(i, _)| i)
                        };
                        Some(byte(*from)..byte(*to))
                    }
                });
            let mut labels = vec![first.label];
            labels.extend(signatures.into_iter().map(|s| s.label));
            return Some(SignatureHelp {
                signatures: labels,
                active_parameter,
            });
        }
        None
    }
}
