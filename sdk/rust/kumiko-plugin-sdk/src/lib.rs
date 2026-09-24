//! Rust SDK for Kumiko plugins (process and wasm kinds).
//!
//! - feature `process`: [`process::Plugin`], a binary speaking KPP over stdin/stdout;
//! - feature `wasm`: [`wasm::Plugin`], a component of the `kumiko:plugin` WIT world.

mod version;

#[cfg(feature = "process")]
pub mod process;
#[cfg(feature = "wasm")]
pub mod wasm;

#[cfg(feature = "process")]
pub use kumiko_ipc::RpcError;

pub use kumiko_protocol as protocol;
pub use kumiko_protocol::{ErrorCode, ResponseError};
pub use kumiko_types as types;
