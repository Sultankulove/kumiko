//! Plugin Host: manifests, registry, supervision, interfaces, kits, KPP bridge.
//!
//! E4 is in progress: manifests, the plugin store, the registry of contributions and the
//! supervision of process plugins with interface routing are in place.

pub mod manifest;
pub mod registry;
pub mod store;
pub mod supervisor;

pub use manifest::{ActivationEvent, Manifest, ManifestError, Permission, PluginKind};
pub use registry::{PluginEntry, PluginRegistry, PluginStatus, RegistryDiff, protocol_version};
pub use store::{InstalledPlugin, PluginStore, Scan, StoreError};
pub use supervisor::{HostApi, PluginHost, Session, SupervisorConfig};
