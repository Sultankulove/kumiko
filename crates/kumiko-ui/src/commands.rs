//! Commands of the workbench itself. Every user action goes through the Command Service
//! (spec, section 20): menus, keys, the project tree and plugins call these ids, and the
//! handlers only forward the request to the UI thread, where the workbench applies it.

use std::sync::Arc;

use kumiko_core::CoreApi;
use kumiko_core::commands::{CommandError, CommandFuture, CommandInfo, Owner};
use kumiko_types::{CommandId, Uri};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::layout::ToolWindow;
use crate::quick_open::Mode;
use crate::tabs::Orientation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Toggle(ToolWindow),
    /// `workbench.openFile [uri]`
    OpenFile(Uri),
    /// Quick Open in a mode: files, commands, a line.
    QuickOpen(Mode),
    CloseEditor,
    /// Split the editor area, or join it back.
    Split(Orientation),
    Unsplit,
    NextEditor,
    PreviousEditor,
    Quit,
}

/// Turns a command's arguments into a request.
type Parse = fn(&[Value]) -> Result<UiCommand, String>;

/// Command ids and how their arguments become a request.
const COMMANDS: &[(&str, Parse)] = &[
    ("workbench.toggleProject", |_| {
        Ok(UiCommand::Toggle(ToolWindow::Project))
    }),
    ("workbench.toggleProblems", |_| {
        Ok(UiCommand::Toggle(ToolWindow::Problems))
    }),
    ("workbench.toggleOutput", |_| {
        Ok(UiCommand::Toggle(ToolWindow::Output))
    }),
    ("workbench.openFile", open_file),
    ("workbench.quickOpen", |_| {
        Ok(UiCommand::QuickOpen(Mode::Files))
    }),
    ("workbench.showCommands", |_| {
        Ok(UiCommand::QuickOpen(Mode::Commands))
    }),
    ("workbench.gotoLine", |_| {
        Ok(UiCommand::QuickOpen(Mode::Line))
    }),
    ("workbench.closeEditor", |_| Ok(UiCommand::CloseEditor)),
    ("workbench.splitRight", |_| {
        Ok(UiCommand::Split(Orientation::Right))
    }),
    ("workbench.splitDown", |_| {
        Ok(UiCommand::Split(Orientation::Down))
    }),
    ("workbench.unsplit", |_| Ok(UiCommand::Unsplit)),
    ("workbench.nextEditor", |_| Ok(UiCommand::NextEditor)),
    ("workbench.previousEditor", |_| {
        Ok(UiCommand::PreviousEditor)
    }),
    ("workbench.quit", |_| Ok(UiCommand::Quit)),
];

fn open_file(args: &[Value]) -> Result<UiCommand, String> {
    let uri = args
        .first()
        .and_then(Value::as_str)
        .ok_or("expected the file's URI")?;
    Uri::parse(uri)
        .map(UiCommand::OpenFile)
        .map_err(|e| e.to_string())
}

/// Registers the workbench commands; their requests arrive on `requests`' receiver.
pub fn register(core: &CoreApi, requests: &UnboundedSender<UiCommand>) {
    for &(id, parse) in COMMANDS {
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
        let handler = Arc::new(move |args: Vec<Value>| -> CommandFuture {
            let sent = parse(&args).and_then(|command| {
                requests
                    .send(command)
                    .map_err(|_| "the workbench is closed".to_owned())
            });
            Box::pin(async move { sent.map(|()| Value::Null).map_err(CommandError::Failed) })
        });
        if let Err(error) = core.commands().register(info, handler) {
            tracing::warn!("{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn open_file_takes_a_uri() {
        assert_eq!(
            open_file(&[json!("file:///w/a.txt")]),
            Ok(UiCommand::OpenFile(Uri::parse("file:///w/a.txt").unwrap()))
        );
        assert!(open_file(&[]).is_err());
        assert!(open_file(&[json!("not a uri")]).is_err());
    }

    #[test]
    fn every_command_has_a_title_in_every_locale() {
        let registry = kumiko_core::resources::ResourceRegistry::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources");
        assert!(registry.scan(&[root]).is_empty());
        for (id, _) in COMMANDS {
            let key = format!("command.{id}");
            let en = registry.localize(&key, "en");
            assert_ne!(en, key, "{id} has no English title");
            // Russian falls back to English when a title is missing.
            assert_ne!(
                registry.localize(&key, "ru"),
                en,
                "{id} has no Russian title"
            );
        }
    }
}
