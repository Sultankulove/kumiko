//! Build, test and run tasks (spec, section 19).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::methods::process::ProcessSpec;
use crate::request;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TaskGroup {
    Build,
    Test,
    Run,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskDefinition {
    /// Unique within the provider.
    pub id: String,
    pub label: String,
    /// Task provider type declared in the manifest.
    #[serde(rename = "type")]
    pub task_type: String,
    pub group: TaskGroup,
    /// What to run; may be left empty by `task/provide` and filled by `task/resolve`.
    pub spec: Option<ProcessSpec>,
    pub problem_matcher: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct TaskExecutionId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskProvideParams {
    #[serde(rename = "type")]
    pub task_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskList {
    pub tasks: Vec<TaskDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskParams {
    pub task: TaskDefinition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskExecution {
    pub execution: TaskExecutionId,
}

request!(
    TaskProvide,
    "task/provide",
    HostToPlugin,
    TaskProvideParams,
    TaskList,
    "List the tasks of a provider type."
);
request!(
    TaskResolve,
    "task/resolve",
    HostToPlugin,
    TaskParams,
    TaskDefinition,
    "Fill in the process spec of a task before it runs."
);
request!(
    TaskRun,
    "task/run",
    PluginToHost,
    TaskParams,
    TaskExecution,
    "Run a task; `task.started` and `task.finished` report its execution."
);
request!(
    TaskCancel,
    "task/cancel",
    PluginToHost,
    TaskExecution,
    (),
    "Cancel a running task execution."
);
