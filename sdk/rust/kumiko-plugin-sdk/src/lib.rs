//! Rust SDK for Kumiko plugins (process and wasm kinds).
//!
//! - feature `process`: [`process::Plugin`], a binary speaking KPP over stdin/stdout;
//! - feature `wasm`: lands in E5.

#[cfg(feature = "process")]
pub mod process;

#[cfg(feature = "process")]
pub use kumiko_ipc::RpcError;

pub use kumiko_protocol as protocol;
pub use kumiko_protocol::{ErrorCode, ResponseError};
pub use kumiko_types as types;
