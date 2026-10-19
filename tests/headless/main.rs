//! Headless scenarios: the IDE driven through CoreApi without UI (spec, section 28).
//! Each stage adds its scenario here.

// Test harness: helpers outside `#[test]` functions may panic too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod e3_documents;
mod e4_plugins;
mod e4_process;
mod e5_wasm;
mod e7_quick_open;
mod e8_syntax;
mod e9_language;
mod fixtures;
