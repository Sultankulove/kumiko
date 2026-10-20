//! Commands of the workbench itself. Every user action goes through the Command Service
//! (spec, section 20): menus, keys, the project tree and plugins call these ids, and the
//! handlers only forward the request to the UI thread, where the workbench applies it.

use std::sync::Arc;

use kumiko_core::CoreApi;
use kumiko_core::commands::{CommandError, CommandFuture, CommandInfo, Owner};
use kumiko_types::{CommandId, Position, Uri};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::layout::ToolWindow;
use crate::quick_open::Mode;
use crate::tabs::Orientation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Toggle(ToolWindow),
    /// `workbench.openFile [uri, line?, character?]`: the position is 0-based, the character a
    /// UTF-8 byte in the line.
    OpenFile(Uri, Option<Position>),
    /// Quick Open in a mode: files, commands, a line.
    QuickOpen(Mode),
    CloseEditor,
    /// Pick a colour theme; set one (`workbench.setTheme [id]`).
    SelectTheme,
    SetTheme(String),
    /// The Settings window.
    OpenSettings,
    /// A command of the editor (see `EditorProvider::commands`), for the active editor.
    Editor(String),
    /// Split the editor area, or join it back.
    Split(Orientation),
    Unsplit,
    NextEditor,
    PreviousEditor,
    /// Language commands at the active editor's caret.
    FindUsages,
    Rename,
    Reformat,
    RunTask,
    StopTasks,
    FindInFiles,
    NewTerminal,
    ApplyKit,
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
    ("workbench.toggleTerminal", |_| {
        Ok(UiCommand::Toggle(ToolWindow::Terminal))
    }),
    ("workbench.newTerminal", |_| Ok(UiCommand::NewTerminal)),
    ("workbench.applyKit", |_| Ok(UiCommand::ApplyKit)),
    ("workbench.toggleCommit", |_| {
        Ok(UiCommand::Toggle(ToolWindow::Commit))
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
    ("workbench.selectTheme", |_| Ok(UiCommand::SelectTheme)),
    ("workbench.openSettings", |_| Ok(UiCommand::OpenSettings)),
    ("workbench.setTheme", set_theme),
    ("workbench.fileStructure", |_| {
        Ok(UiCommand::QuickOpen(Mode::Symbols))
    }),
    ("workbench.findUsages", |_| Ok(UiCommand::FindUsages)),
    ("workbench.rename", |_| Ok(UiCommand::Rename)),
    ("workbench.reformat", |_| Ok(UiCommand::Reformat)),
    ("workbench.runTask", |_| Ok(UiCommand::RunTask)),
    ("workbench.findInFiles", |_| Ok(UiCommand::FindInFiles)),
    ("workbench.stopTasks", |_| Ok(UiCommand::StopTasks)),
    ("workbench.quit", |_| Ok(UiCommand::Quit)),
];

/// Commands that need arguments: the command palette does not offer them.
pub const NEEDS_ARGUMENTS: &[&str] = &["workbench.openFile", "workbench.setTheme"];

fn set_theme(args: &[Value]) -> Result<UiCommand, String> {
    args.first()
        .and_then(Value::as_str)
        .map(|id| UiCommand::SetTheme(id.to_owned()))
        .ok_or_else(|| "expected a theme id".to_owned())
}

fn open_file(args: &[Value]) -> Result<UiCommand, String> {
    let uri = args
        .first()
        .and_then(Value::as_str)
        .ok_or("expected the file's URI")?;
    let uri = Uri::parse(uri).map_err(|e| e.to_string())?;
    let number = |i: usize| {
        args.get(i)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let position = number(1).map(|line| Position::new(line, number(2).unwrap_or(0)));
    Ok(UiCommand::OpenFile(uri, position))
}

/// Registers the workbench commands and the editor ones; their requests arrive on
/// `requests`' receiver.
pub fn register(
    core: &CoreApi,
    requests: &UnboundedSender<UiCommand>,
    editor_commands: &[&'static str],
) {
    for &id in editor_commands {
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
            let sent = requests.send(UiCommand::Editor(id.to_owned())).is_ok();
            Box::pin(async move {
                if sent {
                    Ok(Value::Null)
                } else {
                    Err(CommandError::Failed("the workbench is closed".into()))
                }
            })
        });
        if let Err(error) = core.commands().register(info, handler) {
            tracing::warn!("{error}");
        }
    }
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
            Ok(UiCommand::OpenFile(
                Uri::parse("file:///w/a.txt").unwrap(),
                None
            ))
        );
        assert_eq!(
            open_file(&[json!("file:///w/a.txt"), json!(3), json!(7)]),
            Ok(UiCommand::OpenFile(
                Uri::parse("file:///w/a.txt").unwrap(),
                Some(Position::new(3, 7))
            ))
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
