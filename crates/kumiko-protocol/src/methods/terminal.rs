//! Terminals (spec, section 19): the terminal plugin says what to start; the core runs it in
//! a pseudo-terminal and the UI shows it. The bytes do not pass through the plugin.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::events::TerminalId;
use crate::methods::process::ProcessSpec;
use crate::request;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalOpenParams {
    /// Title of the session's tab.
    pub title: String,
    /// What to run; an empty command is the platform's shell, no cwd the first folder.
    pub spec: ProcessSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalOpenResult {
    pub terminal: TerminalId,
}

request!(
    TerminalOpen,
    "terminal/open",
    PluginToHost,
    TerminalOpenParams,
    TerminalOpenResult,
    "Open a terminal session in the IDE (permission `process.spawn`)."
);
