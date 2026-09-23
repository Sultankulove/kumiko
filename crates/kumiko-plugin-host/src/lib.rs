//! Plugin Host: manifests, registry, supervision, interfaces, kits, KPP bridge.
//!
//! E4 is in progress: manifests are parsed and validated; the rest follows.

pub mod manifest;

pub use manifest::{ActivationEvent, Manifest, ManifestError, Permission, PluginKind};
