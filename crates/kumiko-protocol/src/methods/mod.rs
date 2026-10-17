//! Method catalog by namespace (spec, section 16).
//!
//! Conventions shared by all methods:
//! - field names are camelCase;
//! - optional fields may be omitted or `null`, unknown fields are ignored;
//! - methods without params take no `params` member, methods without a result return `null`.

pub mod base;
pub mod command;
pub mod config;
pub mod document;
pub mod event;
pub mod fs;
pub mod lifecycle;
pub mod lsp;
pub mod process;
pub mod service;
pub mod task;
pub mod terminal;
pub mod ui;
