//! Process service: processes the IDE starts for language servers, tasks and the user, with
//! piped streams and termination of the whole tree (spec, section 19). Each process gets its
//! own process group on Unix and its own Job Object on Windows. PTYs come with the terminal.

#[cfg(windows)]
mod job;

use std::process::Stdio;

use tokio::process::{Child, Command};
use tokio::sync::{Notify, watch};

use kumiko_service_api::{
    BoxFuture, ProcessControl, ProcessError, ProcessSpec, Processes, SpawnedProcess,
};

/// Starts processes on the current Tokio runtime.
#[derive(Debug, Default)]
pub struct ProcessService;

impl ProcessService {
    pub fn new() -> Self {
        Self
    }
}

impl Processes for ProcessService {
    fn spawn(&self, spec: ProcessSpec) -> Result<SpawnedProcess, ProcessError> {
        let failed = |message: String| ProcessError::Spawn {
            command: spec.command.clone(),
            message,
        };
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .envs(&spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|e| failed(e.to_string()))?;
        let pid = child.id().ok_or_else(|| failed("exited at once".into()))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(failed("no standard streams".into()));
        };
        let tree = Tree::new(&child, &spec.owner);
        let kill = std::sync::Arc::new(Notify::new());
        let (exit_tx, exit_rx) = watch::channel(None);
        tokio::spawn(supervise(child, tree, kill.clone(), exit_tx));
        tracing::debug!(owner = %spec.owner, command = %spec.command, pid, "process started");
        Ok(SpawnedProcess {
            pid,
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
            control: Box::new(Control {
                kill,
                exit: exit_rx,
            }),
        })
    }
}

/// `Some(code)` once the process exited; the inner `None` means killed by a signal.
type Exit = Option<Option<i32>>;

/// Waits for the process or for a kill request; the tree goes down either way, so helpers
/// the process started do not outlive it.
async fn supervise(
    mut child: Child,
    tree: Tree,
    kill: std::sync::Arc<Notify>,
    exit: watch::Sender<Exit>,
) {
    let status = tokio::select! {
        status = child.wait() => status.ok(),
        () = kill.notified() => {
            tree.kill();
            let _ = child.kill().await;
            child.wait().await.ok()
        }
    };
    tree.kill();
    let _ = exit.send(Some(status.and_then(|s| s.code())));
}

struct Control {
    kill: std::sync::Arc<Notify>,
    exit: watch::Receiver<Exit>,
}

impl ProcessControl for Control {
    fn kill(&self) {
        self.kill.notify_one();
    }

    fn wait(&self) -> BoxFuture<'_, Option<i32>> {
        let mut exit = self.exit.clone();
        Box::pin(async move {
            match exit.wait_for(Option::is_some).await {
                Ok(code) => code.flatten(),
                Err(_) => None,
            }
        })
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.kill.notify_one();
    }
}

/// The process group (Unix) or job (Windows) of a started process.
struct Tree {
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl Tree {
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn new(child: &Child, owner: &str) -> Self {
        Self {
            #[cfg(unix)]
            group: child
                .id()
                .and_then(|p| i32::try_from(p).ok())
                .and_then(rustix::process::Pid::from_raw),
            #[cfg(windows)]
            job: child
                .raw_handle()
                .and_then(|process| match job::Job::assign(process) {
                    Ok(job) => Some(job),
                    Err(error) => {
                        tracing::warn!(%owner, "no job object, children may outlive kills: {error}");
                        None
                    }
                }),
        }
    }

    fn kill(&self) {
        #[cfg(unix)]
        if let Some(group) = self.group {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// A shell that echoes its input back, on both platforms.
    fn cat() -> ProcessSpec {
        if cfg!(windows) {
            ProcessSpec {
                command: "powershell".into(),
                args: vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    "$input | ForEach-Object { $_ }".into(),
                ],
                owner: "test".into(),
                ..ProcessSpec::default()
            }
        } else {
            ProcessSpec {
                command: "cat".into(),
                owner: "test".into(),
                ..ProcessSpec::default()
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streams_are_piped_and_exit_is_reported() {
        let mut process = ProcessService::new().spawn(cat()).unwrap();
        process.stdin.write_all(b"hello\n").await.unwrap();
        process.stdin.shutdown().await.unwrap();
        drop(process.stdin);
        let mut out = String::new();
        process.stdout.read_to_string(&mut out).await.unwrap();
        assert_eq!(out.trim(), "hello");
        let code = tokio::time::timeout(Duration::from_secs(20), process.control.wait())
            .await
            .unwrap();
        assert_eq!(code, Some(0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_ends_the_process() {
        let process = ProcessService::new().spawn(cat()).unwrap();
        process.control.kill();
        tokio::time::timeout(Duration::from_secs(20), process.control.wait())
            .await
            .unwrap();
    }

    #[test]
    fn a_missing_command_fails_to_spawn() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let spec = ProcessSpec {
            command: "kumiko-no-such-command".into(),
            ..ProcessSpec::default()
        };
        assert!(matches!(
            ProcessService::new().spawn(spec),
            Err(ProcessError::Spawn { .. })
        ));
    }
}
