//! The complete list of methods and core events. Schema, docs and tests are derived from it,
//! so a method that is not listed here does not exist for the rest of the system.

use schemars::{Schema, SchemaGenerator};

use crate::events::{self, CoreEvent, DeliveryClass};
use crate::methods::{
    base, command, config, document, event, fs, lifecycle, lsp, process, service, task, ui,
};
use crate::{Direction, Notification, Request};

/// One-line description attached by the `request!`/`notification!` macros.
pub trait Described {
    const DOC: &'static str;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    Request,
    Notification,
}

type SchemaFn = fn(&mut SchemaGenerator) -> Schema;

/// Catalog entry of one method.
#[derive(Debug, Clone, Copy)]
pub struct MethodInfo {
    pub name: &'static str,
    pub direction: Direction,
    pub kind: MethodKind,
    pub doc: &'static str,
    pub(crate) params_schema: SchemaFn,
    pub(crate) result_schema: Option<SchemaFn>,
}

impl MethodInfo {
    /// Namespace of the method: `document` for `document/read`, `lifecycle` for `initialize`.
    pub fn namespace(&self) -> &'static str {
        match self.name.split_once('/') {
            Some(("state", _)) | None => "lifecycle",
            Some((namespace, _)) => namespace,
        }
    }
}

/// Catalog entry of one core event topic.
#[derive(Debug, Clone, Copy)]
pub struct EventInfo {
    pub topic: &'static str,
    pub class: DeliveryClass,
    pub doc: &'static str,
    pub(crate) payload_schema: SchemaFn,
}

fn schema_of<T: schemars::JsonSchema>(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<T>()
}

fn req<R: Request + Described>() -> MethodInfo {
    MethodInfo {
        name: R::METHOD,
        direction: R::DIRECTION,
        kind: MethodKind::Request,
        doc: R::DOC,
        params_schema: schema_of::<R::Params>,
        result_schema: Some(schema_of::<R::Result>),
    }
}

fn notif<N: Notification + Described>() -> MethodInfo {
    MethodInfo {
        name: N::METHOD,
        direction: N::DIRECTION,
        kind: MethodKind::Notification,
        doc: N::DOC,
        params_schema: schema_of::<N::Params>,
        result_schema: None,
    }
}

fn ev<E: CoreEvent>() -> EventInfo {
    EventInfo {
        topic: E::TOPIC,
        class: E::CLASS,
        doc: E::DOC,
        payload_schema: schema_of::<E>,
    }
}

/// All KPP methods in the order of spec section 16.
pub fn catalog() -> Vec<MethodInfo> {
    vec![
        req::<lifecycle::Initialize>(),
        notif::<lifecycle::Initialized>(),
        req::<lifecycle::Shutdown>(),
        notif::<lifecycle::Exit>(),
        req::<lifecycle::StateSave>(),
        req::<lifecycle::StateRestore>(),
        req::<command::RegisterCommand>(),
        req::<command::ExecuteCommand>(),
        req::<command::SetContext>(),
        req::<command::InvokeCommand>(),
        req::<event::Subscribe>(),
        req::<event::Unsubscribe>(),
        notif::<event::Publish>(),
        notif::<event::EventNotify>(),
        req::<document::ReadDocument>(),
        req::<document::DocumentSnapshotRequest>(),
        req::<document::ApplyEdit>(),
        req::<document::OpenDocument>(),
        req::<document::SaveDocument>(),
        req::<document::ResolveContent>(),
        req::<fs::FsRead>(),
        req::<fs::FsWrite>(),
        req::<fs::FsStat>(),
        req::<fs::FsReadDir>(),
        req::<fs::FsDelete>(),
        req::<fs::FsRename>(),
        req::<fs::FsWatch>(),
        req::<config::ConfigGet>(),
        req::<config::ConfigUpdate>(),
        notif::<config::ConfigChangedNotification>(),
        req::<ui::ShowMessage>(),
        req::<ui::ShowQuickPick>(),
        req::<ui::ShowInput>(),
        req::<ui::SetStatusItem>(),
        req::<ui::TreeSetModel>(),
        req::<ui::TreeUpdate>(),
        req::<ui::TreeGetChildren>(),
        notif::<ui::FileDecorations>(),
        req::<ui::ProgressBegin>(),
        notif::<ui::ProgressReport>(),
        notif::<ui::ProgressEnd>(),
        notif::<ui::ProgressCancel>(),
        req::<ui::OpenDocumentInEditor>(),
        req::<task::TaskProvide>(),
        req::<task::TaskResolve>(),
        req::<task::TaskRun>(),
        req::<task::TaskCancel>(),
        req::<process::Spawn>(),
        req::<process::SpawnPty>(),
        req::<process::ProcessWrite>(),
        req::<process::ProcessResize>(),
        req::<process::ProcessKill>(),
        notif::<process::ProcessExited>(),
        req::<lsp::ResolveLaunch>(),
        req::<lsp::LspMiddleware>(),
        req::<lsp::LspSendRequest>(),
        req::<lsp::LspApplyEdit>(),
        req::<service::ServiceCall>(),
        req::<service::ServiceInvoke>(),
        notif::<base::CancelRequest>(),
        notif::<base::Progress>(),
        notif::<base::Log>(),
        req::<base::Ping>(),
        notif::<base::Resync>(),
    ]
}

/// All core event topics (spec, section 11).
pub fn core_events() -> Vec<EventInfo> {
    vec![
        ev::<events::WorkspaceFolderAdded>(),
        ev::<events::WorkspaceFolderRemoved>(),
        ev::<events::DocumentOpened>(),
        ev::<events::DocumentChanged>(),
        ev::<events::DocumentSaved>(),
        ev::<events::DocumentClosed>(),
        ev::<events::FsChanged>(),
        ev::<events::PluginStateChanged>(),
        ev::<events::TaskStarted>(),
        ev::<events::TaskFinished>(),
        ev::<events::ConfigChanged>(),
        ev::<events::InterfaceAvailabilityChanged>(),
        ev::<events::DiagnosticsChanged>(),
        ev::<events::LanguageStatusChanged>(),
        ev::<events::TerminalOpened>(),
        ev::<events::TerminalClosed>(),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const NAMESPACES: &[&str] = &[
        "lifecycle",
        "command",
        "event",
        "document",
        "fs",
        "config",
        "ui",
        "task",
        "process",
        "lsp",
        "service",
        "$",
    ];

    #[test]
    fn method_names_are_unique_and_namespaced() {
        let methods = catalog();
        let names: BTreeSet<_> = methods.iter().map(|m| m.name).collect();
        assert_eq!(names.len(), methods.len(), "duplicate method name");
        for m in &methods {
            assert!(NAMESPACES.contains(&m.namespace()), "{}", m.name);
            assert!(!m.doc.is_empty(), "{} has no description", m.name);
        }
    }

    #[test]
    fn event_topics_are_unique() {
        let events = core_events();
        let topics: BTreeSet<_> = events.iter().map(|e| e.topic).collect();
        assert_eq!(topics.len(), events.len());
        let lossy: Vec<_> = events
            .iter()
            .filter(|e| e.class == DeliveryClass::Notification)
            .map(|e| e.topic)
            .collect();
        assert_eq!(
            lossy,
            ["fs.changed", "language.statusChanged"],
            "class B: fs.changed, progress and logs (spec, section 11)"
        );
    }
}
