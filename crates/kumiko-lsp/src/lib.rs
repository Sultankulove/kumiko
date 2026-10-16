//! Language service: a generic LSP 3.17 client, routing by language and merging of results
//! (spec, section 18; ADR-005). The editor gets the features through this service and never
//! sees LSP itself.

pub mod client;
pub mod encoding;
pub mod transport;

pub use client::{Client, LspError, ResponseError, ServerHandler};
pub use encoding::Encoding;
