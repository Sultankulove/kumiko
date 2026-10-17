//! E11: Find in Files (MVP criterion 10): matches in the indexed files, ignored folders left
//! out, open documents searched in their unsaved text, the stream stopping at the limit.

use kumiko_headless::kumiko_core::search::text::TextQuery;
use kumiko_headless::{Headless, eventually};
use kumiko_types::{Position, Range, TextEdit, Uri};

async fn all(ide: &Headless, query: TextQuery, limit: usize) -> Vec<(String, u32)> {
    let mut found = ide.core.search_text(&query, limit).unwrap();
    let mut out = Vec::new();
    while let Some(m) = found.recv().await {
        out.push((m.file.relative, m.line));
    }
    out.sort();
    out
}

fn text(t: &str) -> TextQuery {
    TextQuery {
        text: t.into(),
        ..TextQuery::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn find_in_files() {
    let ide = Headless::start().await.unwrap();
    ide.write("src/A.java", "class A {\n  // TODO one\n}\n")
        .unwrap();
    ide.write("src/B.java", "class B { /* todo two */ }\n")
        .unwrap();
    ide.write("target/Gen.java", "// TODO generated\n").unwrap();
    ide.write(".gitignore", "target/\n").unwrap();
    ide.core.open_folder(ide.workspace()).await.unwrap();
    eventually("indexed", || ide.core.file_index().len() >= 3)
        .await
        .unwrap();

    // Ignored folders are not searched; case is ignored unless asked.
    assert_eq!(
        all(&ide, text("todo"), 100).await,
        [("src/A.java".into(), 1), ("src/B.java".into(), 0)]
    );
    let exact = TextQuery {
        case_sensitive: true,
        ..text("TODO")
    };
    assert_eq!(all(&ide, exact, 100).await, [("src/A.java".into(), 1)]);
    assert!(
        ide.core
            .search_text(
                &TextQuery {
                    regex: true,
                    ..text("(")
                },
                10
            )
            .is_err()
    );
    assert_eq!(
        all(&ide, text("class"), 1).await.len(),
        1,
        "the limit stops it"
    );

    // An open document is searched as it is now, not as saved.
    let path = ide.workspace().join("src/B.java");
    let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
    let doc = ide.core.documents().open(&uri, None).await.unwrap();
    ide.core
        .documents()
        .apply(
            doc.id,
            doc.version,
            vec![TextEdit {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                new_text: "// unsaved needle\n".into(),
            }],
        )
        .unwrap();
    assert_eq!(
        all(&ide, text("needle"), 100).await,
        [("src/B.java".into(), 0)]
    );
}
