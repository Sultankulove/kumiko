//! Traits of core services and their shared types.
//!
//! Core services never depend on each other directly: a service receives its neighbours as
//! trait objects from this crate when `kumiko-core` composes them (ADR-019).
//!
//! Stub created in E0; the traits land together with the services in E3 (see docs/spec, section 28).
