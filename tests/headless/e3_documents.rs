//! E3: open a folder → open a file → edit → save → an external change is detected.

use serde_json::json;

use kumiko_headless::kumiko_core::protocol::events::{
    ConfigChanged, DocumentChanged, DocumentSaved, WorkspaceFolderAdded,
};
use kumiko_headless::kumiko_core::protocol::methods::config::ConfigTarget;
use kumiko_headless::kumiko_core::resources::ResourceKind;
use kumiko_headless::{Headless, eventually, wait_for};
use kumiko_types::{DocumentVersion, Position, Range, TextEdit, Uri};

fn edit(line: u32, from: u32, to: u32, text: &str) -> TextEdit {
    TextEdit {
        range: Range::new(Position::new(line, from), Position::new(line, to)),
        new_text: text.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_edit_save_and_detect_external_changes() {
    let ide = Headless::start().await.unwrap();
    let path = ide
        .write("src/hello.txt", "Hello, world!\r\nsecond line\r\n")
        .unwrap();
    let mut events = ide.subscribe(&["workspace.*", "document.*"]);

    // Open the folder.
    let folder = ide.core.open_folder(ide.workspace()).await.unwrap();
    wait_for(&mut events, "workspace.folderAdded", |e| {
        e.decode::<WorkspaceFolderAdded>().is_some()
    })
    .await
    .unwrap();
    assert_eq!(ide.core.project().folders(), [folder]);
    assert!(ide.core.project().contains("src/*.txt").await.unwrap());

    // Open the file.
    let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
    let docs = ide.core.documents();
    let doc = docs.open(&uri, Some("plaintext".into())).await.unwrap();
    assert_eq!(doc.text.to_string(), "Hello, world!\nsecond line\n");
    assert_eq!(doc.version, DocumentVersion(1));

    // Edit (UTF-8 positions) and save.
    let v2 = docs
        .apply(doc.id, doc.version, vec![edit(0, 7, 12, "Kumiko 組子")])
        .unwrap();
    assert!(docs.snapshot(doc.id).unwrap().dirty);
    docs.save(doc.id).await.unwrap();
    wait_for(&mut events, "document.saved", |e| {
        e.decode::<DocumentSaved>()
            .is_some_and(|s| s.unwrap().version == v2)
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "Hello, Kumiko 組子!\r\nsecond line\r\n",
        "saved with the file's CRLF endings"
    );
    assert!(!docs.snapshot(doc.id).unwrap().dirty);

    // Our own save must not come back as an external change; a real one must.
    std::fs::write(&path, "changed outside\n").unwrap();
    let reloaded = wait_for(
        &mut events,
        "document.changed after the external write",
        |e| {
            e.decode::<DocumentChanged>()
                .is_some_and(|c| c.unwrap().version > v2)
        },
    )
    .await
    .unwrap();
    let reloaded = reloaded.decode::<DocumentChanged>().unwrap().unwrap();
    assert_eq!(
        reloaded.version,
        DocumentVersion(v2.0 + 1),
        "exactly one reload"
    );
    let snapshot = docs.snapshot(doc.id).unwrap();
    assert_eq!(snapshot.text.to_string(), "changed outside\n");
    assert!(!snapshot.dirty && !snapshot.conflict);

    // With unsaved edits the user's text stays and a conflict is flagged.
    docs.apply(doc.id, snapshot.version, vec![edit(0, 0, 0, "mine: ")])
        .unwrap();
    std::fs::write(&path, "theirs\n").unwrap();
    eventually("the conflict flag", || {
        docs.snapshot(doc.id).unwrap().conflict
    })
    .await
    .unwrap();
    assert_eq!(
        docs.snapshot(doc.id).unwrap().text.to_string(),
        "mine: changed outside\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_settings_and_resources() {
    let ide = Headless::start().await.unwrap();
    let mut changes = ide.subscribe(&["config.changed"]);
    ide.core.open_folder(ide.workspace()).await.unwrap();

    let config = ide.core.config();
    assert_eq!(
        config.get("editor.tabSize", None),
        Some(json!(4)),
        "schema default"
    );

    // Editing the workspace settings file is picked up by the watcher.
    ide.write(".kumiko/settings.toml", "[editor]\ntabSize = 2\n")
        .unwrap();
    wait_for(&mut changes, "config.changed for editor.tabSize", |e| {
        e.decode::<ConfigChanged>()
            .is_some_and(|c| c.unwrap().keys.contains(&"editor.tabSize".to_owned()))
    })
    .await
    .unwrap();
    assert_eq!(config.get("editor.tabSize", None), Some(json!(2)));

    // Updating through the service writes the user file.
    config
        .update("editor.fontSize", json!(16), ConfigTarget::User)
        .unwrap();
    assert!(
        std::fs::read_to_string(ide.core.dirs().user_settings_file())
            .unwrap()
            .contains("\"editor.fontSize\" = 16")
    );

    // Shipped resources are loaded: themes, translations, the default keymap.
    let resources = ide.core.resources();
    assert_eq!(resources.list(ResourceKind::Theme).len(), 3);
    assert_eq!(resources.localize("command.file.save", "ru"), "Сохранить");
    assert!(
        config
            .keymap()
            .bindings()
            .iter()
            .any(|b| b.command.as_str() == "file.save")
    );
    assert!(config.problems().is_empty(), "{:?}", config.problems());
}
