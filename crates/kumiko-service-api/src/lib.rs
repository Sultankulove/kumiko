//! Traits of core services and their shared types.
//!
//! Core services never depend on each other directly: a service receives its neighbours as
//! trait objects from this crate when `kumiko-core` composes them (ADR-019).

use std::future::Future;
use std::pin::Pin;

mod event;
mod fs;
mod process;
pub mod wasm;
pub mod when;

pub use event::{Event, EventSink};
pub use fs::{FileSystem, FsError, WriteOptions};
pub use kumiko_protocol::events::DeliveryClass;
pub use kumiko_protocol::methods::fs::{DirEntry, FileKind, FileStat};
pub use process::{
    Output, ProcessControl, ProcessError, ProcessSpec, Processes, PtyProcess, PtyProcesses,
    PtySize, SpawnedProcess, Stdin,
};

/// Boxed future used by object-safe async trait methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
