//! FileSystem service: VFS over providers, watcher, ignore rules (spec, section 11).
//!
//! [`LocalFileSystem`] serves `file:` URIs. [`FsWatcher`] watches workspace roots and
//! publishes coalesced `fs.changed` batches (class B) on the event bus.

mod local;
mod watcher;

pub use local::LocalFileSystem;
pub use watcher::{FsWatcher, WatchError, WatcherConfig};
