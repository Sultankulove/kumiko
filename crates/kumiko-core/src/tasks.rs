//! Tasks (spec, section 19): plugins with `[[contributes.taskProviders]]` list them
//! (`task/provide`), the core runs them through the Process service, streams their output
//! into an output channel of their own, and reports `task.started` / `task.finished`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use kumiko_protocol::events::{TaskFinished, TaskStarted};
use kumiko_protocol::methods::task::{TaskDefinition, TaskExecutionId};
use kumiko_service_api::{Event, EventSink, ProcessControl, ProcessSpec};
use kumiko_types::PluginId;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::CoreApi;

/// A task and the plugin that provides it.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub plugin: PluginId,
    pub definition: TaskDefinition,
}

impl Task {
    /// The output channel of its runs.
    pub fn channel(&self) -> String {
        format!("Run: {}", self.definition.label)
    }
}

/// A running task: its label and process.
type Running = (String, Arc<dyn ProcessControl>);

/// Running tasks: their process controls, by execution.
#[derive(Default)]
pub(crate) struct Runs {
    next: AtomicU64,
    running: Mutex<HashMap<TaskExecutionId, Running>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl CoreApi {
    /// The tasks of every ready plugin with a task provider (it starts if needed).
    pub async fn tasks(&self) -> Vec<Task> {
        let providers: Vec<(PluginId, String)> = self
            .plugins()
            .entries()
            .filter(|e| e.is_ready())
            .flat_map(|e| {
                let id = e.manifest().id().clone();
                e.manifest()
                    .contributes
                    .task_providers
                    .iter()
                    .map(move |p| (id.clone(), p.task_type.clone()))
            })
            .collect();
        let mut tasks = Vec::new();
        for (plugin, task_type) in providers {
            match self.plugin_host().task_provide(&plugin, &task_type).await {
                Ok(list) => tasks.extend(list.tasks.into_iter().map(|definition| Task {
                    plugin: plugin.clone(),
                    definition,
                })),
                Err(error) => tracing::warn!(%plugin, %task_type, "no tasks: {}", error.message),
            }
        }
        tasks
    }

    /// Starts a task; its output goes to [`Task::channel`].
    pub async fn run_task(&self, task: Task) -> Result<TaskExecutionId, String> {
        let definition = match task.definition.spec {
            Some(_) => task.definition.clone(),
            None => self
                .plugin_host()
                .task_resolve(&task.plugin, task.definition.clone())
                .await
                .map_err(|e| e.message)?,
        };
        let spec = definition
            .spec
            .clone()
            .ok_or_else(|| format!("task {} has nothing to run", definition.label))?;
        let cwd = spec.cwd.map(std::path::PathBuf::from).or_else(|| {
            self.inner
                .project
                .folders()
                .first()
                .and_then(|f| f.uri.to_file_path())
        });
        let process = self
            .inner
            .processes
            .spawn(ProcessSpec {
                command: spec.command.clone(),
                args: spec.args.clone(),
                cwd,
                env: spec.env.clone(),
                owner: task.plugin.to_string(),
            })
            .map_err(|e| e.to_string())?;

        let runs = &self.inner.runs;
        let execution = TaskExecutionId(runs.next.fetch_add(1, Ordering::Relaxed) + 1);
        let channel = task.channel();
        let output = self.inner.options.output.clone();
        output.clear(&channel);
        output.append(
            &channel,
            &format!("> {} {}", spec.command, spec.args.join(" ")),
        );
        for stream in [
            Box::new(process.stdout) as Box<dyn AsyncRead + Send + Unpin>,
            Box::new(process.stderr),
        ] {
            let (output, channel) = (output.clone(), channel.clone());
            tokio::spawn(async move {
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    output.append(&channel, &line);
                }
            });
        }
        let control: Arc<dyn ProcessControl> = Arc::from(process.control);
        let label = definition.label.clone();
        self.inner.events.publish(Event::core(&TaskStarted {
            execution,
            label: label.clone(),
        }));
        lock(&runs.running).insert(execution, (label, control.clone()));

        let weak = Arc::downgrade(&self.inner);
        let started = Instant::now();
        tokio::spawn(async move {
            let exit_code = control.wait().await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            lock(&inner.runs.running).remove(&execution);
            inner.options.output.append(
                &channel,
                &match exit_code {
                    Some(code) => format!("Process finished with exit code {code}"),
                    None => "Process terminated".to_owned(),
                },
            );
            inner.events.publish(Event::core(&TaskFinished {
                execution,
                exit_code,
                duration_ms: started.elapsed().as_millis() as u64,
            }));
        });
        Ok(execution)
    }

    /// Stops a running task and everything it started.
    pub fn cancel_task(&self, execution: TaskExecutionId) {
        if let Some((_, control)) = lock(&self.inner.runs.running).get(&execution) {
            control.kill();
        }
    }

    /// Running tasks, by execution and label.
    pub fn running_tasks(&self) -> Vec<(TaskExecutionId, String)> {
        let mut list: Vec<_> = lock(&self.inner.runs.running)
            .iter()
            .map(|(id, (label, _))| (*id, label.clone()))
            .collect();
        list.sort();
        list
    }
}

/// Output kept of each stream of [`CoreApi::run_process`].
const MAX_RUN_OUTPUT: usize = 8 * 1024 * 1024;

/// What a process run to its end printed, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

async fn read_all(mut stream: impl AsyncRead + Unpin) -> String {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    while let Ok(n) = stream.read(&mut buffer).await {
        if n == 0 {
            break;
        }
        // Past the limit the stream is still drained, so the process does not block.
        let room = MAX_RUN_OUTPUT.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..n.min(room)]);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

impl CoreApi {
    /// Runs a process to its end (`process/run`): short commands of plugins. The
    /// working folder defaults to the first workspace folder.
    pub async fn run_process(
        &self,
        mut spec: ProcessSpec,
        input: Option<String>,
    ) -> Result<RunOutput, String> {
        use tokio::io::AsyncWriteExt;
        if spec.cwd.is_none() {
            spec.cwd = self
                .inner
                .project
                .folders()
                .first()
                .and_then(|f| f.uri.to_file_path());
        }
        let process = self
            .inner
            .processes
            .spawn(spec)
            .map_err(|e| e.to_string())?;
        let mut stdin = process.stdin;
        tokio::spawn(async move {
            if let Some(input) = input {
                let _ = stdin.write_all(input.as_bytes()).await;
            }
            let _ = stdin.shutdown().await;
        });
        let (stdout, stderr) = tokio::join!(read_all(process.stdout), read_all(process.stderr));
        let exit_code = process.control.wait().await;
        Ok(RunOutput {
            exit_code,
            stdout,
            stderr,
        })
    }
}
