//! Terminals (spec, section 19): shells in pseudo-terminals. The core starts them through the
//! Process service and announces `terminal.opened`; the terminal view takes the output stream
//! and writes keys back. The bytes never pass through a plugin, though `kumiko.terminal`
//! decides what to start (`terminal/open`).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use kumiko_protocol::events::{TerminalClosed, TerminalOpened};
use kumiko_service_api::{Event, EventSink, ProcessControl, ProcessSpec, PtySize};

use crate::CoreApi;

pub use kumiko_protocol::events::TerminalId;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A running terminal: its input, size and process; the output goes to whoever takes it.
pub struct Terminal {
    pub id: TerminalId,
    pub title: String,
    input: Mutex<Box<dyn Write + Send>>,
    output: Mutex<Option<Box<dyn Read + Send>>>,
    resize: Box<dyn Fn(PtySize) + Send + Sync>,
    control: Box<dyn ProcessControl>,
}

impl Terminal {
    /// Keys and pasted text.
    pub fn write(&self, bytes: &[u8]) {
        let mut input = lock(&self.input);
        if input.write_all(bytes).and_then(|()| input.flush()).is_err() {
            tracing::debug!(id = self.id.0, "terminal input closed");
        }
    }

    pub fn resize(&self, size: PtySize) {
        (self.resize)(size);
    }

    /// The output stream, once: the view that shows the terminal reads it.
    pub fn take_output(&self) -> Option<Box<dyn Read + Send>> {
        lock(&self.output).take()
    }

    /// The shell's exit code when it has ended.
    pub async fn wait(&self) -> Option<i32> {
        self.control.wait().await
    }
}

#[derive(Default)]
pub(crate) struct Terminals {
    next: AtomicU64,
    open: Mutex<BTreeMap<TerminalId, Arc<Terminal>>>,
}

/// The shell of this platform: `$SHELL` or `/bin/bash`, PowerShell on Windows.
fn default_shell() -> String {
    if cfg!(windows) {
        "powershell.exe".to_owned()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_owned())
    }
}

impl CoreApi {
    /// Starts a shell in a terminal; an empty `spec.command` takes `terminal.shell`, then the
    /// platform's shell. The working folder defaults to the first workspace folder.
    pub fn open_terminal(
        &self,
        title: &str,
        mut spec: ProcessSpec,
        size: PtySize,
    ) -> Result<TerminalId, String> {
        if spec.command.trim().is_empty() {
            spec.command = self
                .inner
                .config
                .get("terminal.shell", None)
                .and_then(|v| v.as_str().map(str::to_owned))
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(default_shell);
        }
        if spec.cwd.is_none() {
            spec.cwd = self
                .inner
                .project
                .folders()
                .first()
                .and_then(|f| f.uri.to_file_path());
        }
        // Programs check this to know they may use colours.
        spec.env
            .entry("TERM".to_owned())
            .or_insert_with(|| "xterm-256color".to_owned());
        let process = self
            .inner
            .pty
            .spawn_pty(spec, size)
            .map_err(|e| e.to_string())?;
        let id = TerminalId(self.inner.terminals.next.fetch_add(1, Ordering::Relaxed) + 1);
        let terminal = Arc::new(Terminal {
            id,
            title: title.to_owned(),
            input: Mutex::new(process.input),
            output: Mutex::new(Some(process.output)),
            resize: process.resize,
            control: process.control,
        });
        lock(&self.inner.terminals.open).insert(id, terminal.clone());
        self.inner.events.publish(Event::core(&TerminalOpened {
            terminal: id,
            title: title.to_owned(),
        }));
        // Gone from the list when the shell ends.
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let exit_code = terminal.wait().await;
            if let Some(inner) = weak.upgrade() {
                lock(&inner.terminals.open).remove(&id);
                inner.events.publish(Event::core(&TerminalClosed {
                    terminal: id,
                    exit_code,
                }));
            }
        });
        Ok(id)
    }

    pub fn terminal(&self, id: TerminalId) -> Option<Arc<Terminal>> {
        lock(&self.inner.terminals.open).get(&id).cloned()
    }

    /// Open terminals, oldest first.
    pub fn terminals(&self) -> Vec<(TerminalId, String)> {
        lock(&self.inner.terminals.open)
            .values()
            .map(|t| (t.id, t.title.clone()))
            .collect()
    }

    /// Ends the shell (and what it started).
    pub fn close_terminal(&self, id: TerminalId) {
        if let Some(terminal) = self.terminal(id) {
            terminal.control.kill();
        }
    }
}
