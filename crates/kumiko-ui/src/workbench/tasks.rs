//! Run Task (Alt+Shift+F10): the tasks plugins provide, in a picker; the chosen one runs
//! and its output shows in the Output tool window. Ctrl+F2 stops what runs.

use gpui::Context;

use super::{FocusTarget, Workbench};
use crate::layout::ToolWindow;
use crate::quick_open::{Action, Item, Mode, QuickOpen};

impl Workbench {
    /// Asks the plugins for their tasks (they may start), then offers them.
    pub(super) fn open_tasks(&mut self, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let task = self.runtime.spawn(async move { core.tasks().await });
        cx.spawn(async move |this, cx| {
            let Ok(tasks) = task.await else {
                return;
            };
            let _ = this.update(cx, |workbench, cx| {
                if tasks.is_empty() {
                    workbench.message = Some(workbench.t("tasks.none"));
                    cx.notify();
                    return;
                }
                workbench.task_list = tasks;
                workbench.open_menu = None;
                workbench.quick_open = Some(QuickOpen::picker(Mode::Tasks));
                workbench.refresh_quick();
                workbench.focus_request = Some(FocusTarget::Quick);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn task_items(&self, query: &str) -> Vec<Item> {
        let query = query.to_lowercase();
        self.task_list
            .iter()
            .enumerate()
            .filter(|(_, t)| t.definition.label.to_lowercase().contains(&query))
            .map(|(i, t)| Item {
                label: t.definition.label.clone(),
                highlights: Vec::new(),
                detail: t.plugin.to_string(),
                keys: None,
                action: Action::RunTask(i),
            })
            .collect()
    }

    pub(super) fn run_task(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(task) = self.task_list.get(index).cloned() else {
            return;
        };
        let channel = task.channel();
        let core = self.core.clone();
        let run = self.runtime.spawn(async move { core.run_task(task).await });
        cx.spawn(async move |this, cx| {
            let Ok(result) = run.await else {
                return;
            };
            let _ = this.update(cx, |workbench, cx| {
                match result {
                    Ok(_) => {
                        // The run's channel, in the Output tool window.
                        workbench.output_channel = Some(channel);
                        if !workbench.layout.is_open(ToolWindow::Output) {
                            workbench.layout.toggle(ToolWindow::Output);
                        }
                        workbench.refresh_output();
                    }
                    Err(error) => workbench.message = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stops every running task.
    pub(super) fn stop_tasks(&mut self) {
        for (execution, _) in self.core.running_tasks() {
            self.core.cancel_task(execution);
        }
    }
}
