//! Plugin Host: manifests, registry, supervision, interfaces, kits, KPP bridge.
//!
//! E4 is in progress: manifests are parsed and validated, plugins are installed into the store and
//! resolved into a registry of contributions; supervision and the KPP bridge follow.

pub mod manifest;
pub mod registry;
pub mod store;

pub use manifest::{ActivationEvent, Manifest, ManifestError, Permission, PluginKind};
pub use registry::{PluginEntry, PluginRegistry, PluginStatus, protocol_version};
pub use store::{InstalledPlugin, PluginStore, Scan, StoreError};
