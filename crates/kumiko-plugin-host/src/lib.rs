//! Plugin Host: manifests, registry, supervision, interfaces, kits, KPP bridge.
//!
//! E4 is in progress: manifests are parsed and validated, plugins are installed into the store;
//! the rest follows.

pub mod manifest;
pub mod store;

pub use manifest::{ActivationEvent, Manifest, ManifestError, Permission, PluginKind};
pub use store::{InstalledPlugin, PluginStore, Scan, StoreError};
