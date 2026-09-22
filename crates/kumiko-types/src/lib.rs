//! Shared primitive types: Uri, DocumentId, Position, Range, TextEdit, ids, errors, paths.
//!
//! This crate is the vocabulary of every other crate and of the plugin protocol, so it has
//! no internal dependencies and no async runtime.

mod ids;
pub mod paths;
mod text;
mod uri;

pub use ids::{CommandId, DocumentId, IdError, InterfaceId, PluginId};
pub use paths::{KumikoDirs, PathsError};
pub use text::{DocumentVersion, Position, Range, Selection, TextEdit};
pub use uri::{Uri, UriError};
