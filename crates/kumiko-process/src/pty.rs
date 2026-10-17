//! Pseudo-terminals (spec, section 19): ConPTY on Windows, Unix PTYs elsewhere, through
//! portable-pty. The terminal view reads and writes the bytes; nothing is parsed here.

use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, PtySize as Size, native_pty_system};
use tokio::sync::watch;

use kumiko_service_api::{
    BoxFuture, ProcessControl, ProcessError, ProcessSpec, PtyProcess, PtyProcesses, PtySize,
};

use crate::ProcessService;

fn size(size: PtySize) -> Size {
    Size {
        rows: size.rows.max(1),
        cols: size.cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl PtyProcesses for ProcessService {
    fn spawn_pty(&self, spec: ProcessSpec, initial: PtySize) -> Result<PtyProcess, ProcessError> {
        let failed = |message: String| ProcessError::Spawn {
            command: spec.command.clone(),
            message,
        };
        let pair = native_pty_system()
            .openpty(size(initial))
            .map_err(|e| failed(e.to_string()))?;
        let mut command = CommandBuilder::new(&spec.command);
        command.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            command.cwd(cwd);
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| failed(e.to_string()))?;
        // The slave end belongs to the child now; keeping it open would keep the output
        // from ending when the child exits.
        drop(pair.slave);
        let output = pair
            .master
            .try_clone_reader()
            .map_err(|e| failed(e.to_string()))?;
        let input = pair
            .master
            .take_writer()
            .map_err(|e| failed(e.to_string()))?;
        let pid = child.process_id();
        let killer = Mutex::new(child.clone_killer());
        let (exit_tx, exit) = watch::channel(None);
        std::thread::Builder::new()
            .name("kumiko-pty-wait".into())
            .spawn(move || {
                let code = child.wait().ok().map(|s| s.exit_code() as i32);
                let _ = exit_tx.send(Some(code));
            })
            .map_err(|e| failed(e.to_string()))?;
        let master = Arc::new(Mutex::new(pair.master));
        tracing::debug!(owner = %spec.owner, command = %spec.command, ?pid, "terminal started");
        Ok(PtyProcess {
            pid,
            output,
            input,
            resize: Box::new(move |new| {
                let master = master.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(error) = master.resize(size(new)) {
                    tracing::debug!("terminal resize: {error}");
                }
            }),
            control: Box::new(PtyControl { killer, exit }),
        })
    }
}

type Killer = Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>;

struct PtyControl {
    killer: Killer,
    exit: watch::Receiver<Option<Option<i32>>>,
}

impl ProcessControl for PtyControl {
    fn kill(&self) {
        let _ = self.killer.lock().unwrap_or_else(|e| e.into_inner()).kill();
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

impl Drop for PtyControl {
    fn drop(&mut self) {
        if self.exit.borrow().is_none() {
            self.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::time::Duration;

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shell_in_a_pty_echoes_and_exits() {
        let spec = if cfg!(windows) {
            ProcessSpec {
                command: "cmd.exe".into(),
                args: vec!["/c".into(), "echo pty-ok".into()],
                ..ProcessSpec::default()
            }
        } else {
            ProcessSpec {
                command: "sh".into(),
                args: vec!["-c".into(), "echo pty-ok".into()],
                ..ProcessSpec::default()
            }
        };
        let process = ProcessService::new()
            .spawn_pty(spec, PtySize { cols: 80, rows: 24 })
            .unwrap();
        (process.resize)(PtySize {
            cols: 100,
            rows: 30,
        });
        let mut output = process.output;
        let mut input = process.input;
        let reader = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut buffer = [0u8; 4096];
            let mut answered = false;
            // Drained to the end: ConPTY stalls a child whose output nobody reads, and ends the
            // stream only when the pseudo-terminal closes.
            while let Ok(n) = output.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buffer[..n]);
                // ConPTY asks where the cursor is before it runs the child; a terminal answers.
                if !answered && String::from_utf8_lossy(&seen).contains("\x1b[6n") {
                    answered = true;
                    let _ = input.write_all(b"\x1b[1;1R");
                    let _ = input.flush();
                }
            }
            String::from_utf8_lossy(&seen).into_owned()
        });
        let code = tokio::time::timeout(Duration::from_secs(20), process.control.wait())
            .await
            .unwrap();
        assert_eq!(code, Some(0));
        // Closing the terminal ends its output.
        drop(process.resize);
        let seen = reader.join().unwrap();
        assert!(seen.contains("pty-ok"), "{seen:?}");
    }
}
