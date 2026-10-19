//! Language service: a generic LSP 3.17 client, routing by language and merging of results
//! (spec, section 18; ADR-005). The editor gets the features through this service and never
//! sees LSP itself.

pub mod client;
mod convert;
pub mod encoding;
mod service;
pub mod transport;
pub mod types;

pub use client::{Client, LspError, ResponseError, ServerHandler};
pub use encoding::Encoding;
pub use service::{
    LanguageHost, LanguageService, MAX_RESTARTS, Progress, ServerDecl, ServerKey, ServerStatus,
};
pub use types::{
    CodeAction, CompletionItem, CompletionKind, Diagnostic, Highlight, Hover, Location, Severity,
    SignatureHelp, Symbol, WorkspaceEdit,
};
