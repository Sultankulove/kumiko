//! The default editor view (L1, spec, section 12): it shows a document of the Document
//! Service and turns input into transactions. The editing model (selections, movement,
//! commands, display map) is plain data with tests; the GPUI element draws it.
//!
//! What was taken from other editors and why: docs/research/editor-prior-art.md.

pub mod selection;
pub mod text;
