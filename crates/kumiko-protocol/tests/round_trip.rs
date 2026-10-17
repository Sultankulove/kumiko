//! Serde round-trip of every message in the catalog, both as a bare payload and inside the
//! JSON-RPC envelope. The test fails if a catalog method or core event has no sample.

// Test harness: helpers outside `#[test]` functions may panic too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use kumiko_protocol::common::*;
use kumiko_protocol::events::{self, CoreEvent};
use kumiko_protocol::methods::*;
use kumiko_protocol::schema::protocol_schema;
use kumiko_protocol::{
    Message, Notification, NotificationMessage, Request, RequestId, RequestMessage,
    ResponseMessage, catalog,
};
use kumiko_types::{
    CommandId, DocumentVersion, InterfaceId, PluginId, Position, Range, TextEdit, Uri,
};

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
    let json = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, value, "round trip changed the value: {json}");
}

#[derive(Default)]
struct Samples {
    methods: BTreeSet<&'static str>,
    events: BTreeSet<&'static str>,
}

impl Samples {
    fn req<R: Request>(&mut self, params: R::Params, result: R::Result)
    where
        R::Params: PartialEq + Debug,
        R::Result: PartialEq + Debug,
    {
        round_trip(&params);
        round_trip(&result);

        let id = RequestId::String(format!("req-{}", R::METHOD));
        let request = RequestMessage::new::<R>(id.clone(), &params).unwrap();
        let Message::Request(request) =
            Message::from_slice(&Message::Request(request).to_vec()).unwrap()
        else {
            panic!("{} did not come back as a request", R::METHOD);
        };
        assert_eq!(request.params::<R>().unwrap().unwrap(), params);

        let response = ResponseMessage::ok::<R>(id.clone(), &result).unwrap();
        let Message::Response(response) =
            Message::from_slice(&Message::Response(response).to_vec()).unwrap()
        else {
            panic!("{} did not come back as a response", R::METHOD);
        };
        assert_eq!(response.id, Some(id));
        assert_eq!(response.result::<R>().unwrap().unwrap(), result);

        assert!(
            self.methods.insert(R::METHOD),
            "{} sampled twice",
            R::METHOD
        );
    }

    fn notif<N: Notification>(&mut self, params: N::Params)
    where
        N::Params: PartialEq + Debug,
    {
        round_trip(&params);
        let notification = NotificationMessage::new::<N>(&params).unwrap();
        let Message::Notification(notification) =
            Message::from_slice(&Message::Notification(notification).to_vec()).unwrap()
        else {
            panic!("{} did not come back as a notification", N::METHOD);
        };
        assert_eq!(notification.params::<N>().unwrap().unwrap(), params);
        assert!(
            self.methods.insert(N::METHOD),
            "{} sampled twice",
            N::METHOD
        );
    }

    fn event<E: CoreEvent + PartialEq + Debug>(&mut self, payload: E) {
        round_trip(&payload);
        assert!(self.events.insert(E::TOPIC), "{} sampled twice", E::TOPIC);
    }
}

fn uri(path: &str) -> Uri {
    Uri::parse(format!("file:///work/{path}")).unwrap()
}

fn range(l1: u32, c1: u32, l2: u32, c2: u32) -> Range {
    Range::new(Position::new(l1, c1), Position::new(l2, c2))
}

fn call(command: &str) -> CommandCall {
    CommandCall {
        command: CommandId::new(command).unwrap(),
        args: vec![json!({"flag": true}), json!(3)],
    }
}

fn plugin() -> PluginId {
    PluginId::new("org.example.sample").unwrap()
}

fn folder() -> WorkspaceFolder {
    WorkspaceFolder {
        uri: uri(""),
        name: "work".into(),
    }
}

fn spec() -> process::ProcessSpec {
    process::ProcessSpec {
        command: "make".into(),
        args: vec!["test".into()],
        cwd: Some("/work".into()),
        env: BTreeMap::from([("CI".into(), "1".into())]),
        env_mode: process::EnvMode::Replace,
        pty: Some(process::PtySize {
            cols: 120,
            rows: 40,
        }),
        kill_on_owner_exit: true,
    }
}

fn task_def() -> task::TaskDefinition {
    task::TaskDefinition {
        id: "unit".into(),
        label: "Unit tests".into(),
        task_type: "make".into(),
        group: task::TaskGroup::Test,
        spec: Some(spec()),
        problem_matcher: None,
    }
}

fn node(id: &str, children: Option<Vec<ui::TreeNode>>) -> ui::TreeNode {
    ui::TreeNode {
        id: id.into(),
        label: id.to_uppercase(),
        icon: Some("folder".into()),
        description: None,
        tooltip: Some("tip".into()),
        collapsible: if children.is_some() {
            ui::Collapsible::Expanded
        } else {
            ui::Collapsible::Collapsed
        },
        context_value: Some("item".into()),
        command: Some(call("view.open")),
        children,
    }
}

fn all_samples() -> Samples {
    let mut s = Samples::default();
    let v3 = DocumentVersion(3);
    let doc = || document::DocumentParams { uri: uri("a.txt") };

    // lifecycle
    s.req::<lifecycle::Initialize>(
        lifecycle::InitializeParams {
            api_version: "0.1.0".into(),
            plugin_id: plugin(),
            host_capabilities: lifecycle::HostCapabilities {
                methods: vec!["document/read".into()],
                experimental: Some(json!({"x": 1})),
            },
            workspace: lifecycle::WorkspaceInfo {
                folders: vec![folder()],
            },
            config: json!({"sample.enabled": true}),
            locale: "ru".into(),
            storage: Some(lifecycle::PluginStorage {
                data: "/d/sample".into(),
                cache: "/c/sample".into(),
            }),
        },
        lifecycle::InitializeResult {
            api_version: "0.1.0".into(),
            plugin_capabilities: lifecycle::PluginCapabilities {
                methods: vec!["state/save".into()],
                lsp_middleware: vec!["workspace/executeCommand".into()],
                experimental: None,
            },
        },
    );
    s.notif::<lifecycle::Initialized>(());
    s.req::<lifecycle::Shutdown>((), ());
    s.notif::<lifecycle::Exit>(());
    s.req::<lifecycle::StateSave>(
        (),
        lifecycle::SavedState {
            state: json!({"open": [1, 2]}),
        },
    );
    s.req::<lifecycle::StateRestore>(
        lifecycle::SavedState {
            state: json!("opaque"),
        },
        (),
    );

    // command
    s.req::<command::RegisterCommand>(
        command::RegisterCommandParams {
            command: CommandId::new("sample.run").unwrap(),
        },
        (),
    );
    s.req::<command::ExecuteCommand>(call("file.save"), json!({"saved": true}));
    s.req::<command::SetContext>(
        command::SetContextParams {
            key: "sample.ready".into(),
            value: json!(true),
        },
        (),
    );
    s.req::<command::InvokeCommand>(call("sample.run"), Value::Null);

    // event
    s.req::<event::Subscribe>(
        event::SubscribeParams {
            topics: vec!["document.*".into(), "fs.changed".into()],
            glob: Some("**/*.toml".into()),
        },
        event::SubscribeResult {
            subscription: SubscriptionId(9),
        },
    );
    s.req::<event::Unsubscribe>(
        event::UnsubscribeParams {
            subscription: SubscriptionId(9),
        },
        (),
    );
    s.notif::<event::Publish>(event::PublishParams {
        topic: "sample.indexed".into(),
        payload: json!({"files": 10}),
    });
    s.notif::<event::EventNotify>(event::EventNotifyParams {
        subscription: SubscriptionId(9),
        topic: "document.saved".into(),
        payload: json!({"uri": "file:///work/a.txt", "version": 3}),
    });

    // document
    s.req::<document::ReadDocument>(
        doc(),
        document::DocumentText {
            uri: uri("a.txt"),
            version: None,
            text: "hello\n".into(),
        },
    );
    let snapshot = document::DocumentSnapshot {
        uri: uri("a.txt"),
        version: v3,
        language_id: Some("plaintext".into()),
        text: "héllo\n".into(),
    };
    s.req::<document::DocumentSnapshotRequest>(doc(), snapshot.clone());
    s.req::<document::ApplyEdit>(
        document::ApplyEditParams {
            uri: uri("a.txt"),
            base_version: v3,
            edits: vec![TextEdit {
                range: range(0, 0, 0, 1),
                new_text: "H".into(),
            }],
        },
        document::ApplyEditResult { version: v3.next() },
    );
    s.req::<document::OpenDocument>(doc(), snapshot);
    s.req::<document::SaveDocument>(doc(), ());
    s.req::<document::ResolveContent>(
        document::DocumentParams {
            uri: Uri::parse("sample:/virtual/1").unwrap(),
        },
        document::ResolvedContent {
            text: "generated".into(),
            language_id: None,
        },
    );

    // fs
    s.req::<fs::FsRead>(
        doc(),
        fs::FsReadResult {
            content: Content::Base64("AAEC".into()),
        },
    );
    s.req::<fs::FsWrite>(
        fs::FsWriteParams {
            uri: uri("b.txt"),
            content: Content::Text("text".into()),
            create: true,
            overwrite: false,
        },
        (),
    );
    s.req::<fs::FsStat>(
        doc(),
        fs::FileStat {
            kind: fs::FileKind::File,
            size: 6,
            mtime_ms: Some(1_758_600_000_000),
            readonly: false,
        },
    );
    s.req::<fs::FsReadDir>(
        document::DocumentParams { uri: uri("") },
        fs::FsReadDirResult {
            entries: vec![
                fs::DirEntry {
                    name: "src".into(),
                    kind: fs::FileKind::Directory,
                },
                fs::DirEntry {
                    name: "link".into(),
                    kind: fs::FileKind::Symlink,
                },
            ],
        },
    );
    s.req::<fs::FsDelete>(
        fs::FsDeleteParams {
            uri: uri("tmp"),
            recursive: true,
        },
        (),
    );
    s.req::<fs::FsRename>(
        fs::FsRenameParams {
            from: uri("a.txt"),
            to: uri("c.txt"),
            overwrite: false,
        },
        (),
    );
    s.req::<fs::FsWatch>(
        fs::FsWatchParams {
            glob: "**/*.toml".into(),
            base: None,
        },
        event::SubscribeResult {
            subscription: SubscriptionId(10),
        },
    );

    // config
    s.req::<config::ConfigGet>(
        config::ConfigGetParams {
            key: "editor.fontSize".into(),
            scope: Some(config::ConfigScope {
                uri: Some(uri("a.txt")),
                language_id: Some("plaintext".into()),
            }),
        },
        json!(14),
    );
    s.req::<config::ConfigUpdate>(
        config::ConfigUpdateParams {
            key: "sample.level".into(),
            value: Value::Null,
            target: config::ConfigTarget::Workspace,
        },
        (),
    );
    s.notif::<config::ConfigChangedNotification>(events::ConfigChanged {
        keys: vec!["sample.level".into()],
    });

    // ui
    s.req::<ui::ShowMessage>(
        ui::ShowMessageParams {
            level: ui::MessageLevel::Warning,
            message: "Restart?".into(),
            actions: vec!["Yes".into(), "No".into()],
        },
        ui::ShowMessageResult {
            action: Some("Yes".into()),
        },
    );
    s.req::<ui::ShowQuickPick>(
        ui::ShowQuickPickParams {
            items: vec![ui::QuickPickItem {
                id: "a".into(),
                label: "Alpha".into(),
                description: Some("first".into()),
                detail: None,
                picked: true,
            }],
            placeholder: Some("Pick".into()),
            can_pick_many: true,
        },
        Some(ui::QuickPickResult {
            selected: vec!["a".into()],
        }),
    );
    s.req::<ui::ShowInput>(
        ui::ShowInputParams {
            prompt: Some("Password".into()),
            password: true,
            ..Default::default()
        },
        None,
    );
    s.req::<ui::SetStatusItem>(
        ui::SetStatusItemParams {
            id: "status".into(),
            item: Some(ui::StatusItem {
                text: "Ready".into(),
                tooltip: None,
                command: Some(call("sample.run")),
                alignment: ui::StatusAlignment::Right,
                priority: -5,
            }),
        },
        (),
    );
    s.req::<ui::TreeSetModel>(
        ui::TreeSetModelParams {
            view_id: "sample.tree".into(),
            roots: vec![node("root", Some(vec![node("leaf", None)]))],
        },
        (),
    );
    s.req::<ui::TreeUpdate>(
        ui::TreeUpdateParams {
            view_id: "sample.tree".into(),
            node_id: Some("root".into()),
            children: None,
        },
        (),
    );
    s.req::<ui::TreeGetChildren>(
        ui::TreeGetChildrenParams {
            view_id: "sample.tree".into(),
            node_id: "leaf".into(),
        },
        ui::TreeChildren {
            children: vec![node("deeper", None)],
        },
    );
    s.notif::<ui::FileDecorations>(ui::FileDecorationsParams {
        set: vec![ui::FileDecoration {
            uri: uri("a.txt"),
            badge: Some("M".into()),
            color: Some("sample.modified".into()),
            tooltip: None,
        }],
        clear: vec![uri("c.txt")],
    });
    let token = || ProgressToken("index-1".into());
    s.req::<ui::ProgressBegin>(
        ui::ProgressBeginParams {
            token: token(),
            title: "Indexing".into(),
            message: None,
            percentage: Some(0),
            cancellable: true,
        },
        (),
    );
    s.notif::<ui::ProgressReport>(ui::ProgressReportParams {
        token: token(),
        message: Some("10/100".into()),
        percentage: Some(10),
    });
    s.notif::<ui::ProgressEnd>(ui::ProgressEndParams {
        token: token(),
        message: None,
    });
    s.notif::<ui::ProgressCancel>(ui::ProgressCancelParams { token: token() });
    s.req::<ui::OpenDocumentInEditor>(
        ui::OpenDocumentInEditorParams {
            uri: uri("a.txt"),
            selection: Some(range(2, 0, 2, 4)),
            preview: true,
        },
        (),
    );

    // task
    s.req::<task::TaskProvide>(
        task::TaskProvideParams {
            task_type: "make".into(),
        },
        task::TaskList {
            tasks: vec![task::TaskDefinition {
                spec: None,
                ..task_def()
            }],
        },
    );
    s.req::<task::TaskResolve>(task::TaskParams { task: task_def() }, task_def());
    s.req::<task::TaskRun>(
        task::TaskParams { task: task_def() },
        task::TaskExecution {
            execution: task::TaskExecutionId(5),
        },
    );
    s.req::<task::TaskCancel>(
        task::TaskExecution {
            execution: task::TaskExecutionId(5),
        },
        (),
    );

    // process
    let pid = process::ProcessId(77);
    s.req::<process::Spawn>(
        process::SpawnParams {
            spec: process::ProcessSpec {
                pty: None,
                ..spec()
            },
        },
        process::SpawnResult {
            process: pid,
            stdout: StreamId(1),
            stderr: StreamId(2),
        },
    );
    s.req::<process::SpawnPty>(
        process::SpawnParams { spec: spec() },
        process::SpawnPtyResult {
            process: pid,
            output: StreamId(3),
        },
    );
    s.req::<process::ProcessWrite>(
        process::ProcessWriteParams {
            process: pid,
            data: Content::Text("ls\n".into()),
        },
        (),
    );
    s.req::<process::ProcessResize>(
        process::ProcessResizeParams {
            process: pid,
            size: process::PtySize { cols: 80, rows: 24 },
        },
        (),
    );
    s.req::<process::ProcessKill>(
        process::ProcessKillParams {
            process: pid,
            signal: process::Signal::Terminate,
        },
        (),
    );
    s.notif::<process::ProcessExited>(process::ProcessExitedParams {
        process: pid,
        exit_code: None,
        signal: Some(15),
    });

    // lsp
    s.req::<lsp::ResolveLaunch>(
        lsp::ResolveLaunchParams {
            server_id: "sample-ls".into(),
            language_id: "sample".into(),
            workspace_folder: uri(""),
        },
        lsp::LaunchSpec {
            command: "/opt/sample-ls/bin/server".into(),
            args: vec!["--stdio".into()],
            env: BTreeMap::new(),
            cwd: None,
            initialization_options: Some(json!({"settings": {}})),
        },
    );
    let lsp_message = lsp::LspMessageParams {
        server_id: "sample-ls".into(),
        method: "workspace/executeCommand".into(),
        params: json!({"command": "sample.apply"}),
    };
    s.req::<lsp::LspMiddleware>(
        lsp_message.clone(),
        lsp::MiddlewareResult::Forward {
            params: json!({"command": "sample.apply", "arguments": []}),
        },
    );
    s.req::<lsp::LspSendRequest>(lsp_message, json!([1, 2, 3]));

    // service
    let interface = InterfaceId::new("org.example.repository").unwrap();
    s.req::<service::ServiceCall>(
        service::ServiceCallParams {
            interface: interface.clone(),
            method: "status".into(),
            params: json!({}),
        },
        json!({"branch": "main"}),
    );
    s.req::<service::ServiceInvoke>(
        service::ServiceInvokeParams {
            interface,
            method: "status".into(),
            params: json!({}),
            caller: plugin(),
        },
        json!({"branch": "main"}),
    );

    // $/
    s.notif::<base::CancelRequest>(base::CancelParams {
        id: RequestId::Number(42),
    });
    s.notif::<base::Progress>(base::ProgressParams {
        token: token(),
        value: json!({"partial": [1]}),
    });
    s.notif::<base::Log>(base::LogParams {
        level: base::LogLevel::Debug,
        message: "started".into(),
        target: Some("sample::init".into()),
    });
    s.req::<base::Ping>((), ());
    s.notif::<base::Resync>(base::ResyncParams {
        topics: vec!["document.changed".into()],
    });

    // core events
    s.event(events::WorkspaceFolderAdded { folder: folder() });
    s.event(events::WorkspaceFolderRemoved { folder: folder() });
    s.event(events::DocumentOpened {
        uri: uri("a.txt"),
        version: DocumentVersion(1),
        language_id: None,
    });
    s.event(events::DocumentChanged {
        uri: uri("a.txt"),
        version: DocumentVersion(2),
        changes: vec![TextEdit {
            range: range(0, 5, 0, 5),
            new_text: "!".into(),
        }],
    });
    s.event(events::DocumentSaved {
        uri: uri("a.txt"),
        version: DocumentVersion(2),
    });
    s.event(events::DocumentClosed { uri: uri("a.txt") });
    s.event(events::FsChanged {
        changes: vec![events::FsChange {
            uri: uri("target/x"),
            kind: events::FsChangeKind::Deleted,
        }],
    });
    s.event(events::PluginStateChanged {
        plugin: plugin(),
        state: events::PluginState::Crashed,
        reason: Some("exit code 137".into()),
    });
    s.event(events::TaskStarted {
        execution: task::TaskExecutionId(5),
        label: "Unit tests".into(),
    });
    s.event(events::TaskFinished {
        execution: task::TaskExecutionId(5),
        exit_code: Some(0),
        duration_ms: 1234,
    });
    s.event(events::ConfigChanged {
        keys: vec!["editor.fontSize".into()],
    });
    s.event(events::InterfaceAvailabilityChanged {
        interface: InterfaceId::new("org.example.repository").unwrap(),
        available: false,
    });
    s.event(events::DiagnosticsChanged {
        uris: vec![Uri::parse("file:///w/src/A.txt").unwrap()],
    });
    s.event(events::LanguageStatusChanged {});
    s
}

#[test]
fn every_catalog_method_and_event_round_trips() {
    let samples = all_samples();
    let catalog: BTreeSet<_> = catalog().iter().map(|m| m.name).collect();
    let missing: Vec<_> = catalog.difference(&samples.methods).collect();
    assert!(
        missing.is_empty(),
        "methods without a round-trip sample: {missing:?}"
    );

    let schema = protocol_schema();
    let topics: BTreeSet<&str> = schema["events"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let sampled: BTreeSet<&str> = samples.events.iter().copied().collect();
    assert_eq!(topics, sampled, "core events without a round-trip sample");
}

#[test]
fn optional_fields_may_be_omitted() {
    // Other-language SDKs may leave out optional members instead of sending null.
    let params: ui::ShowQuickPickParams = serde_json::from_value(json!({"items": []})).unwrap();
    assert_eq!(params.placeholder, None);
    assert!(!params.can_pick_many);
    let spec: process::ProcessSpec = serde_json::from_value(json!({"command": "ls"})).unwrap();
    assert_eq!(spec.env_mode, process::EnvMode::Merge);
    assert!(spec.args.is_empty());
}

#[test]
fn wire_names_are_camel_case() {
    let json = serde_json::to_value(task_def()).unwrap();
    assert_eq!(json["type"], "make");
    assert!(json.get("problemMatcher").is_some());
    assert_eq!(json["spec"]["killOnOwnerExit"], true);
    assert_eq!(json["spec"]["envMode"], "replace");
    let middleware = serde_json::to_value(lsp::MiddlewareResult::Respond {
        result: json!(null),
    })
    .unwrap();
    assert_eq!(middleware, json!({"action": "respond", "result": null}));
    assert_eq!(
        serde_json::to_value(Content::Base64("AA==".into())).unwrap(),
        json!({"base64": "AA=="})
    );
}
