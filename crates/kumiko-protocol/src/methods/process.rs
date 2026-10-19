//! Processes started by the core Process Service on behalf of a plugin (permission
//! `process.spawn`). Output travels in binary streams, not in JSON.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::common::{Content, StreamId};
use crate::{notification, request};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvMode {
    /// Add `env` to the IDE environment.
    #[default]
    Merge,
    /// Use only `env`.
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

/// What to run. The owner is the calling plugin; the host records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Absolute working directory; the first workspace folder when omitted.
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub env_mode: EnvMode,
    /// Run in a pseudo-terminal of this size.
    pub pty: Option<PtySize>,
    /// Kill the process tree when the owning plugin stops.
    #[serde(default)]
    pub kill_on_owner_exit: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ProcessId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpawnParams {
    pub spec: ProcessSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpawnResult {
    pub process: ProcessId,
    pub stdout: StreamId,
    pub stderr: StreamId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpawnPtyResult {
    pub process: ProcessId,
    /// Terminal output.
    pub output: StreamId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessWriteParams {
    pub process: ProcessId,
    /// Bytes for stdin (or the terminal input).
    pub data: Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessResizeParams {
    pub process: ProcessId,
    pub size: PtySize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Signal {
    Interrupt,
    Terminate,
    Kill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessKillParams {
    pub process: ProcessId,
    /// Sent to the whole process group.
    pub signal: Signal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExitedParams {
    pub process: ProcessId,
    pub exit_code: Option<i32>,
    /// Number of the signal that terminated the process.
    pub signal: Option<i32>,
}

request!(
    Spawn,
    "process/spawn",
    PluginToHost,
    SpawnParams,
    SpawnResult,
    "Start a process with piped output."
);
request!(
    SpawnPty,
    "process/spawnPty",
    PluginToHost,
    SpawnParams,
    SpawnPtyResult,
    "Start a process in a pseudo-terminal (`spec.pty` is required)."
);
request!(
    ProcessWrite,
    "process/write",
    PluginToHost,
    ProcessWriteParams,
    (),
    "Write to the process input."
);
request!(
    ProcessResize,
    "process/resize",
    PluginToHost,
    ProcessResizeParams,
    (),
    "Resize the pseudo-terminal."
);
request!(
    ProcessKill,
    "process/kill",
    PluginToHost,
    ProcessKillParams,
    (),
    "Signal the process tree."
);
notification!(
    ProcessExited,
    "process/exited",
    HostToPlugin,
    ProcessExitedParams,
    "A process started by the plugin exited; its output streams are closed before this."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunParams {
    pub spec: ProcessSpec,
    /// Written to the process input, which is then closed.
    #[serde(default)]
    pub input: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    /// `null` when killed by a signal.
    pub exit_code: Option<i32>,
    /// Output as UTF-8 (invalid bytes replaced), each cut at 8 MiB.
    pub stdout: String,
    pub stderr: String,
}

request!(
    Run,
    "process/run",
    PluginToHost,
    RunParams,
    RunResult,
    "Run a process to its end and return its output (for short commands, e.g. a status query)."
);
