use std::collections::HashMap;

use kumiko_service_api::{BoxFuture, DirEntry, FileStat};

use super::*;

/// In-memory `file:` storage.
#[derive(Default)]
struct MemoryFs {
    files: Mutex<HashMap<Uri, Vec<u8>>>,
}

impl MemoryFs {
    fn put(&self, uri: &Uri, bytes: &[u8]) {
        lock(&self.files).insert(uri.clone(), bytes.to_vec());
    }

    fn get(&self, uri: &Uri) -> Option<Vec<u8>> {
        lock(&self.files).get(uri).cloned()
    }

    fn remove(&self, uri: &Uri) {
        lock(&self.files).remove(uri);
    }
}

fn unsupported<'a, T: Send + 'a>(uri: &Uri) -> BoxFuture<'a, Result<T, FsError>> {
    let error = FsError::Unsupported(uri.clone());
    Box::pin(async move { Err(error) })
}

impl FileSystem for MemoryFs {
    fn read<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<Vec<u8>, FsError>> {
        let result = self.get(uri).ok_or_else(|| FsError::NotFound(uri.clone()));
        Box::pin(async move { result })
    }

    fn write<'a>(
        &'a self,
        uri: &'a Uri,
        data: Vec<u8>,
        _: WriteOptions,
    ) -> BoxFuture<'a, Result<(), FsError>> {
        self.put(uri, &data);
        Box::pin(async { Ok(()) })
    }

    fn stat<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<FileStat, FsError>> {
        unsupported(uri)
    }

    fn read_dir<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<Vec<DirEntry>, FsError>> {
        unsupported(uri)
    }

    fn create_dir<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<(), FsError>> {
        unsupported(uri)
    }

    fn delete<'a>(&'a self, uri: &'a Uri, _: bool) -> BoxFuture<'a, Result<(), FsError>> {
        unsupported(uri)
    }

    fn rename<'a>(
        &'a self,
        from: &'a Uri,
        _: &'a Uri,
        _: bool,
    ) -> BoxFuture<'a, Result<(), FsError>> {
        unsupported(from)
    }

    fn copy<'a>(
        &'a self,
        from: &'a Uri,
        _: &'a Uri,
        _: bool,
    ) -> BoxFuture<'a, Result<(), FsError>> {
        unsupported(from)
    }
}

#[derive(Default)]
struct Collect(Mutex<Vec<Event>>);

impl EventSink for Collect {
    fn publish(&self, event: Event) {
        lock(&self.0).push(event);
    }
}

impl Collect {
    fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *lock(&self.0))
    }

    fn topics(&self) -> Vec<String> {
        self.take().into_iter().map(|e| e.topic).collect()
    }
}

struct Fixture {
    fs: Arc<MemoryFs>,
    events: Arc<Collect>,
    docs: DocumentService,
}

fn fixture(config: DocumentsConfig) -> Fixture {
    let fs = Arc::new(MemoryFs::default());
    let events = Arc::new(Collect::default());
    let docs = DocumentService::new(fs.clone(), events.clone(), config);
    Fixture { fs, events, docs }
}

fn uri(name: &str) -> Uri {
    Uri::parse(format!("file:///w/{name}")).unwrap()
}

fn edit(l1: u32, c1: u32, l2: u32, c2: u32, text: &str) -> TextEdit {
    TextEdit {
        range: Range::new(Position::new(l1, c1), Position::new(l2, c2)),
        new_text: text.to_owned(),
    }
}

fn v(n: u64) -> DocumentVersion {
    DocumentVersion(n)
}

#[tokio::test]
async fn open_edit_save_round_trip() {
    let f = fixture(DocumentsConfig::default());
    let a = uri("a.txt");
    f.fs.put(&a, b"hello\r\nworld\r\n");

    let doc = f.docs.open(&a, Some("plaintext".into())).await.unwrap();
    assert_eq!(doc.version, v(1));
    assert_eq!(doc.text.to_string(), "hello\nworld\n");
    assert!(!doc.dirty);
    assert_eq!(
        f.docs.open(&a, None).await.unwrap().id,
        doc.id,
        "opened once"
    );

    let version = f
        .docs
        .apply(doc.id, v(1), vec![edit(1, 0, 1, 5, "wörld")])
        .unwrap();
    assert_eq!(version, v(2));
    let snapshot = f.docs.snapshot(doc.id).unwrap();
    assert!(snapshot.dirty);
    assert_eq!(snapshot.text.to_string(), "hello\nwörld\n");

    f.docs.save(doc.id).await.unwrap();
    assert_eq!(
        f.fs.get(&a).unwrap(),
        "hello\r\nwörld\r\n".as_bytes(),
        "CRLF restored"
    );
    assert!(!f.docs.snapshot(doc.id).unwrap().dirty);

    f.docs.close(doc.id).unwrap();
    assert!(f.docs.find(&a).is_none());
    assert_eq!(
        f.events.topics(),
        [
            "document.opened",
            "document.changed",
            "document.saved",
            "document.closed"
        ]
    );
}

#[tokio::test]
async fn change_events_carry_versions_and_edits() {
    let f = fixture(DocumentsConfig::default());
    let a = uri("a.txt");
    f.fs.put(&a, b"ab");
    let id = f.docs.open(&a, None).await.unwrap().id;
    f.events.take();
    f.docs
        .apply(id, v(1), vec![edit(0, 0, 0, 0, "<"), edit(0, 2, 0, 2, ">")])
        .unwrap();
    let events = f.events.take();
    let change = events[0].decode::<DocumentChanged>().unwrap().unwrap();
    assert_eq!(change.version, v(2));
    assert_eq!(
        change.changes,
        [edit(0, 2, 0, 2, ">"), edit(0, 0, 0, 0, "<")]
    );
}

#[tokio::test]
async fn stale_edits_are_rebased_or_rejected() {
    let f = fixture(DocumentsConfig::default());
    let a = uri("a.rs");
    f.fs.put(&a, b"fn a() {}\nfn b() {}\n");
    let id = f.docs.open(&a, None).await.unwrap().id;

    // Someone inserts a line at the top (v2); an edit computed on v1 still lands right.
    f.docs
        .apply(id, v(1), vec![edit(0, 0, 0, 0, "// top\n")])
        .unwrap();
    f.docs
        .apply(id, v(1), vec![edit(1, 3, 1, 4, "beta")])
        .unwrap();
    assert_eq!(
        f.docs.snapshot(id).unwrap().text.to_string(),
        "// top\nfn a() {}\nfn beta() {}\n"
    );

    // An edit on v1 overlapping text changed since then is refused.
    assert_eq!(
        f.docs.apply(id, v(1), vec![edit(1, 0, 1, 5, "x")]),
        Err(DocError::ContentModified)
    );
    assert!(matches!(
        f.docs.apply(id, v(9), vec![]),
        Err(DocError::VersionAhead { .. })
    ));
}

#[tokio::test]
async fn rebasing_needs_the_history() {
    let config = DocumentsConfig {
        history_len: 2,
        ..DocumentsConfig::default()
    };
    let f = fixture(config);
    let a = uri("a.txt");
    f.fs.put(&a, b"x");
    let id = f.docs.open(&a, None).await.unwrap().id;
    for n in 1..=3 {
        f.docs.apply(id, v(n), vec![edit(0, 0, 0, 0, "y")]).unwrap();
    }
    assert!(f.docs.apply(id, v(2), vec![edit(0, 0, 0, 0, "z")]).is_ok());
    assert_eq!(
        f.docs.apply(id, v(1), vec![edit(0, 0, 0, 0, "z")]),
        Err(DocError::ContentModified)
    );
}

#[tokio::test]
async fn back_to_the_stored_text_is_clean() {
    // Every edit its own undo step.
    let f = fixture(DocumentsConfig {
        undo_group_window: Duration::ZERO,
        ..DocumentsConfig::default()
    });
    let a = uri("a.txt");
    f.fs.put(&a, b"hello\n");
    let id = f.docs.open(&a, None).await.unwrap().id;
    let dirty = |f: &Fixture| f.docs.snapshot(id).unwrap().dirty;

    // Undo of everything since opening.
    f.docs.apply(id, v(1), vec![edit(0, 5, 0, 5, "!")]).unwrap();
    assert!(dirty(&f));
    f.docs.undo(id).unwrap();
    assert!(!dirty(&f), "undone to the stored text");
    f.docs.redo(id).unwrap();
    assert!(dirty(&f));

    // Typed back by hand, and the same after a save.
    f.docs.apply(id, v(4), vec![edit(0, 5, 0, 6, "")]).unwrap();
    assert!(!dirty(&f));
    f.docs.apply(id, v(5), vec![edit(0, 0, 0, 0, "x")]).unwrap();
    f.docs.save(id).await.unwrap();
    f.docs.apply(id, v(6), vec![edit(0, 0, 0, 1, "")]).unwrap();
    assert!(dirty(&f), "differs from what was saved");
    f.docs.undo(id).unwrap();
    assert!(!dirty(&f));
}

#[tokio::test]
async fn undo_groups_by_time_and_redo() {
    let f = fixture(DocumentsConfig {
        undo_group_window: Duration::from_secs(60),
        ..DocumentsConfig::default()
    });
    let a = uri("a.txt");
    f.fs.put(&a, b"");
    let id = f.docs.open(&a, None).await.unwrap().id;
    let text = |f: &Fixture| f.docs.snapshot(id).unwrap().text.to_string();

    f.docs
        .apply(id, v(1), vec![edit(0, 0, 0, 0, "ab")])
        .unwrap();
    f.docs.apply(id, v(2), vec![edit(0, 2, 0, 2, "c")]).unwrap();
    assert_eq!(text(&f), "abc");

    f.docs.undo(id).unwrap();
    assert_eq!(text(&f), "", "both edits were one group");
    f.docs.redo(id).unwrap();
    assert_eq!(text(&f), "abc");
    f.docs.undo(id).unwrap();
    assert_eq!(f.docs.undo(id).unwrap(), None);
    f.docs.redo(id).unwrap();
    assert_eq!(f.docs.redo(id).unwrap(), None);
    assert_eq!(text(&f), "abc");
}

#[tokio::test]
async fn separate_undo_steps_and_new_edits_clear_redo() {
    let f = fixture(DocumentsConfig {
        undo_group_window: Duration::ZERO,
        ..DocumentsConfig::default()
    });
    let a = uri("a.txt");
    f.fs.put(&a, b"1\n2\n3");
    let id = f.docs.open(&a, None).await.unwrap().id;
    let text = |f: &Fixture| f.docs.snapshot(id).unwrap().text.to_string();

    // A multi-cursor edit is one step.
    f.docs
        .apply(
            id,
            v(1),
            vec![edit(0, 0, 0, 1, "one"), edit(2, 0, 2, 1, "three")],
        )
        .unwrap();
    f.docs.apply(id, v(2), vec![edit(1, 0, 1, 1, "")]).unwrap();
    assert_eq!(text(&f), "one\n\nthree");

    f.docs.undo(id).unwrap();
    assert_eq!(text(&f), "one\n2\nthree");
    f.docs.undo(id).unwrap();
    assert_eq!(text(&f), "1\n2\n3");

    f.docs.redo(id).unwrap();
    let version = f.docs.snapshot(id).unwrap().version;
    f.docs
        .apply(id, version, vec![edit(0, 0, 0, 0, ">")])
        .unwrap();
    assert_eq!(f.docs.redo(id).unwrap(), None, "a new edit clears redo");
    assert_eq!(text(&f), ">one\n2\nthree");
}

#[tokio::test]
async fn external_changes() {
    let f = fixture(DocumentsConfig::default());
    let a = uri("a.txt");
    f.fs.put(&a, b"v1");
    let id = f.docs.open(&a, None).await.unwrap().id;

    // Our own save is not an external change.
    f.docs.apply(id, v(1), vec![edit(0, 1, 0, 2, "2")]).unwrap();
    f.docs.save(id).await.unwrap();
    assert_eq!(
        f.docs.external_change(&a).await.unwrap(),
        ExternalChange::Unchanged
    );

    // A clean document follows the file.
    f.fs.put(&a, b"from outside");
    let ExternalChange::Reloaded(version) = f.docs.external_change(&a).await.unwrap() else {
        panic!("expected a reload");
    };
    let snapshot = f.docs.snapshot(id).unwrap();
    assert_eq!((snapshot.version, snapshot.dirty), (version, false));
    assert_eq!(snapshot.text.to_string(), "from outside");
    f.docs.undo(id).unwrap();
    assert_eq!(
        f.docs.snapshot(id).unwrap().text.to_string(),
        "v2",
        "reload is undoable"
    );
    f.docs.redo(id).unwrap();

    // A dirty one keeps the user's text and reports a conflict.
    let version = f.docs.snapshot(id).unwrap().version;
    f.docs
        .apply(id, version, vec![edit(0, 0, 0, 0, "mine: ")])
        .unwrap();
    f.fs.put(&a, b"theirs");
    assert_eq!(
        f.docs.external_change(&a).await.unwrap(),
        ExternalChange::Conflict
    );
    let snapshot = f.docs.snapshot(id).unwrap();
    assert!(snapshot.conflict && snapshot.dirty);
    assert_eq!(snapshot.text.to_string(), "mine: from outside");
    f.docs.revert(id).await.unwrap();
    let snapshot = f.docs.snapshot(id).unwrap();
    assert_eq!(snapshot.text.to_string(), "theirs");
    assert!(!snapshot.conflict && !snapshot.dirty);

    f.fs.remove(&a);
    assert_eq!(
        f.docs.external_change(&a).await.unwrap(),
        ExternalChange::Deleted
    );
    assert!(f.docs.snapshot(id).unwrap().conflict);
    assert_eq!(
        f.docs.external_change(&uri("closed.txt")).await.unwrap(),
        ExternalChange::NotOpen
    );
}

#[tokio::test]
async fn size_limits() {
    let f = fixture(DocumentsConfig {
        large_file_bytes: 4,
        read_only_bytes: 8,
        ..DocumentsConfig::default()
    });
    let (large, huge) = (uri("large.txt"), uri("huge.txt"));
    f.fs.put(&large, b"123456");
    f.fs.put(&huge, b"1234567890");
    let large = f.docs.open(&large, None).await.unwrap();
    assert!(large.large && !large.read_only);
    let huge = f.docs.open(&huge, None).await.unwrap();
    assert!(huge.read_only);
    assert!(matches!(
        f.docs.apply(huge.id, v(1), vec![edit(0, 0, 0, 0, "x")]),
        Err(DocError::ReadOnly(_))
    ));
}

#[tokio::test]
async fn latin1_file_rejects_unencodable_text() {
    let f = fixture(DocumentsConfig::default());
    let a = uri("legacy.txt");
    f.fs.put(&a, b"caf\xE9");
    let id = f.docs.open(&a, None).await.unwrap().id;
    f.docs
        .apply(id, v(1), vec![edit(0, 5, 0, 5, " €")])
        .unwrap();
    assert!(matches!(
        f.docs.save(id).await,
        Err(DocError::Unencodable {
            character: '€', ..
        })
    ));
    assert_eq!(f.fs.get(&a).unwrap(), b"caf\xE9", "file untouched");
}

#[tokio::test]
async fn missing_files_and_documents() {
    let f = fixture(DocumentsConfig::default());
    let missing = uri("missing.txt");
    assert_eq!(
        f.docs.open(&missing, None).await.unwrap_err(),
        DocError::Fs(FsError::NotFound(missing))
    );
    assert_eq!(
        f.docs.apply(DocumentId(42), v(1), vec![]),
        Err(DocError::NotOpen(DocumentId(42)))
    );
}
