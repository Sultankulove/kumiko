use std::collections::BTreeMap;
use std::path::PathBuf;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::BoxFuture;

/// What to start (spec, section 19). PTYs come with the terminal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Added to the IDE's environment.
    pub env: BTreeMap<String, String>,
    /// Plugin id or `user`, for logs and for killing what a plugin left behind.
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessError {
    #[error("cannot start {command}: {message}")]
    Spawn { command: String, message: String },
}

pub type Stdin = Box<dyn AsyncWrite + Send + Unpin>;
pub type Output = Box<dyn AsyncRead + Send + Unpin>;

/// A running process with piped standard streams. Dropping the control kills the tree.
pub struct SpawnedProcess {
    pub pid: u32,
    pub stdin: Stdin,
    pub stdout: Output,
    pub stderr: Output,
    pub control: Box<dyn ProcessControl>,
}

pub trait ProcessControl: Send + Sync {
    /// Kills the process and everything it started.
    fn kill(&self);
    /// The exit code once the process has exited (`None` when killed by a signal).
    fn wait(&self) -> BoxFuture<'_, Option<i32>>;
}

/// The Process service: every process the IDE starts for plugins, servers and the user.
pub trait Processes: Send + Sync {
    fn spawn(&self, spec: ProcessSpec) -> Result<SpawnedProcess, ProcessError>;
}
