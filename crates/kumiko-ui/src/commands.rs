//! Commands of the workbench itself. Every user action goes through the Command Service
//! (spec, section 20): menus, keys and plugins call these ids, and the handlers only forward
//! the request to the UI thread, where the workbench applies it.

use std::sync::Arc;

use kumiko_core::CoreApi;
use kumiko_core::commands::{CommandFuture, CommandInfo, Owner};
use kumiko_types::CommandId;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::layout::ToolWindow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    Toggle(ToolWindow),
    Quit,
}

impl UiCommand {
    pub fn all() -> impl Iterator<Item = UiCommand> {
        ToolWindow::ALL
            .into_iter()
            .map(UiCommand::Toggle)
            .chain([UiCommand::Quit])
    }

    pub fn id(self) -> &'static str {
        match self {
            UiCommand::Toggle(window) => window.toggle_command(),
            UiCommand::Quit => "workbench.quit",
        }
    }
}

/// Registers the workbench commands; their requests arrive on `requests`' receiver.
pub fn register(core: &CoreApi, requests: &UnboundedSender<UiCommand>) {
    for command in UiCommand::all() {
        let id = command.id();
        let Ok(command_id) = CommandId::new(id) else {
            continue;
        };
        let info = CommandInfo {
            id: command_id,
            title: Some(format!("%command.{id}%")),
            owner: Owner::Core,
            when: None,
        };
        let requests = requests.clone();
        let handler = Arc::new(move |_args: Vec<Value>| -> CommandFuture {
            let sent = requests.send(command).is_ok();
            Box::pin(async move {
                if sent {
                    Ok(Value::Null)
                } else {
                    Err(kumiko_core::commands::CommandError::Failed(
                        "the workbench is closed".into(),
                    ))
                }
            })
        });
        if let Err(error) = core.commands().register(info, handler) {
            tracing::warn!("{error}");
        }
    }
}
