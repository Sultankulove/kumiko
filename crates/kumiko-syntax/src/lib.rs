//! Syntax service (spec, section 18): Tree-sitter grammars from L3 packages, one syntax tree
//! per open document kept in step with its edits (incremental reparse), highlighting of byte
//! ranges and folding regions.
//!
//! The service knows no language: grammars, their file extensions and queries are data.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use kumiko_types::{DocumentId, DocumentVersion, Position, TextEdit};
use ropey::Rope;
use tree_sitter::{InputEdit, Parser, Point, QueryCursor, StreamingIterator, Tree};

mod grammar;

pub use grammar::{Grammar, GrammarManifest};

/// A grammar package to load: its directory and the `[grammar]` table of its manifest.
pub struct GrammarPackage<'a> {
    pub dir: &'a Path,
    pub grammar: &'a serde_json::Value,
}

/// A highlighted byte range and the capture name (`keyword`, `function.method`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlight {
    pub range: Range<usize>,
    pub capture: Arc<str>,
}

struct DocumentSyntax {
    language: String,
    grammar: Arc<Grammar>,
    version: DocumentVersion,
    rope: Rope,
    tree: Option<Tree>,
}

#[derive(Default)]
pub struct SyntaxService {
    grammars: RwLock<HashMap<String, Arc<Grammar>>>,
    documents: Mutex<HashMap<DocumentId, DocumentSyntax>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Ropes are fed to Tree-sitter chunk by chunk, without copying the text.
fn parse(parser: &mut Parser, rope: &Rope, old: Option<&Tree>) -> Option<Tree> {
    let len = rope.len_bytes();
    parser.parse_with_options(
        &mut |byte: usize, _point: Point| {
            if byte >= len {
                return &[][..];
            }
            let (chunk, start, _, _) = rope.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - start..]
        },
        old,
        None,
    )
}

fn point(position: Position) -> Point {
    Point::new(position.line as usize, position.character as usize)
}

/// Where `text` inserted at `start` ends.
fn end_after(start: Position, text: &str) -> Position {
    match text.rfind('\n') {
        None => Position::new(start.line, start.character + text.len() as u32),
        Some(last) => Position::new(
            start.line + text.matches('\n').count() as u32,
            (text.len() - last - 1) as u32,
        ),
    }
}

fn byte_of(rope: &Rope, position: Position) -> usize {
    let line = (position.line as usize).min(rope.len_lines().saturating_sub(1));
    (rope.line_to_byte(line) + position.character as usize).min(rope.len_bytes())
}

impl SyntaxService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads grammar packages; returns the problems of those that failed.
    pub fn load(&self, packages: &[GrammarPackage<'_>]) -> Vec<String> {
        let mut problems = Vec::new();
        for package in packages {
            let manifest: GrammarManifest = match serde_json::from_value(package.grammar.clone()) {
                Ok(manifest) => manifest,
                Err(error) => {
                    problems.push(format!("{}: [grammar]: {error}", package.dir.display()));
                    continue;
                }
            };
            match Grammar::load(package.dir, manifest) {
                Ok(grammar) => {
                    let language = grammar.manifest.language.clone();
                    self.grammars
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(language, Arc::new(grammar));
                }
                Err(error) => problems.push(format!("{}: {error}", package.dir.display())),
            }
        }
        problems
    }

    /// Languages with a grammar.
    pub fn languages(&self) -> Vec<String> {
        let mut list: Vec<String> = self
            .grammars
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect();
        list.sort();
        list
    }

    /// The language whose grammar claims the file's extension.
    pub fn language_for_path(&self, path: &Path) -> Option<String> {
        let extension = path.extension()?.to_str()?.to_lowercase();
        self.grammars
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .find(|g| {
                g.manifest
                    .extensions
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(&extension))
            })
            .map(|g| g.manifest.language.clone())
    }

    /// Brings the document's tree to `version`. `changes_since` gives the edits of every
    /// version after the one the tree has (oldest first, each in the coordinates of the text
    /// before it), or `None` when they are no longer known: then the text is parsed anew.
    pub fn sync(
        &self,
        document: DocumentId,
        language: &str,
        version: DocumentVersion,
        rope: &Rope,
        changes_since: impl FnOnce(DocumentVersion) -> Option<Vec<Vec<TextEdit>>>,
    ) -> bool {
        let Some(grammar) = self
            .grammars
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(language)
            .cloned()
        else {
            lock(&self.documents).remove(&document);
            return false;
        };
        let mut documents = lock(&self.documents);
        let current = documents
            .get(&document)
            .filter(|d| d.language == language && d.version <= version);
        if current.is_some_and(|d| d.version == version) {
            return true;
        }
        let mut parser = Parser::new();
        if parser.set_language(&grammar.language).is_err() {
            return false;
        }
        // Incremental: replay the edits on the old tree, then reparse with it.
        let incremental = current.and_then(|d| {
            let steps = changes_since(d.version)?;
            let mut tree = d.tree.clone()?;
            let mut text = d.rope.clone();
            for edits in steps {
                // Edits of one version are descending and refer to the text before them.
                for edit in &edits {
                    let start_byte = byte_of(&text, edit.range.start);
                    let old_end_byte = byte_of(&text, edit.range.end);
                    let new_end = end_after(edit.range.start, &edit.new_text);
                    tree.edit(&InputEdit {
                        start_byte,
                        old_end_byte,
                        new_end_byte: start_byte + edit.new_text.len(),
                        start_position: point(edit.range.start),
                        old_end_position: point(edit.range.end),
                        new_end_position: point(new_end),
                    });
                    let from = text.byte_to_char(start_byte);
                    let to = text.byte_to_char(old_end_byte);
                    text.remove(from..to);
                    text.insert(from, &edit.new_text);
                }
            }
            (text.len_bytes() == rope.len_bytes()).then_some(tree)
        });
        let tree = parse(&mut parser, rope, incremental.as_ref());
        documents.insert(
            document,
            DocumentSyntax {
                language: language.to_owned(),
                grammar,
                version,
                rope: rope.clone(),
                tree,
            },
        );
        true
    }

    pub fn close(&self, document: DocumentId) {
        lock(&self.documents).remove(&document);
    }

    /// Forgets the trees of documents no longer open.
    pub fn retain(&self, open: &[DocumentId]) {
        lock(&self.documents).retain(|id, _| open.contains(id));
    }

    /// Highlights intersecting `range` (bytes), in order; nested captures follow their
    /// parents, so later ones should win when painting.
    pub fn highlights(&self, document: DocumentId, range: Range<usize>) -> Vec<Highlight> {
        let documents = lock(&self.documents);
        let Some(doc) = documents.get(&document) else {
            return Vec::new();
        };
        let (Some(tree), Some(query)) = (&doc.tree, &doc.grammar.highlights) else {
            return Vec::new();
        };
        let names: Vec<Arc<str>> = query
            .capture_names()
            .iter()
            .map(|n| Arc::from(*n))
            .collect();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range);
        let rope = &doc.rope;
        let text = |node: tree_sitter::Node<'_>| {
            let range = node.byte_range();
            std::iter::once(rope.byte_slice(range).to_string().into_bytes())
        };
        let mut out = Vec::new();
        let mut captures = cursor.captures(query, tree.root_node(), text);
        while let Some((m, index)) = captures.next() {
            let capture = m.captures()[*index];
            out.push(Highlight {
                range: capture.node.byte_range(),
                capture: names[capture.index as usize].clone(),
            });
        }
        out
    }

    /// Foldable regions as `(first line, last line)`, spanning at least two lines.
    pub fn folds(&self, document: DocumentId) -> Vec<(usize, usize)> {
        let documents = lock(&self.documents);
        let Some(doc) = documents.get(&document) else {
            return Vec::new();
        };
        let (Some(tree), Some(query)) = (&doc.tree, &doc.grammar.folds) else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        let rope = &doc.rope;
        let text = |node: tree_sitter::Node<'_>| {
            std::iter::once(rope.byte_slice(node.byte_range()).to_string().into_bytes())
        };
        let mut folds = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), text);
        while let Some(m) = matches.next() {
            for capture in m.captures() {
                let (start, end) = (
                    capture.node.start_position().row,
                    capture.node.end_position().row,
                );
                if end > start {
                    folds.push((start, end));
                }
            }
        }
        folds.sort_unstable();
        folds.dedup();
        folds
    }
}
