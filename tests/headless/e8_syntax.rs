//! E8: Java highlighting and folding come from the grammar package (spec, section 18): a
//! native parser library loaded at start, its queries, an incremental tree per document.

use kumiko_headless::Headless;
use kumiko_types::{Position, Range, TextEdit, Uri};

use crate::fixtures;

/// The parser library of `resources/grammars/java`, built next to the test binaries where
/// the Syntax service looks for it in development builds.
fn build_grammar() {
    fixtures::build(
        "kumiko-grammar-java",
        &[],
        "kumiko_grammar_java",
        std::env::consts::DLL_EXTENSION,
    );
}

const SOURCE: &str = "public class Hello {\n    // greet\n    void greet() {\n        String s = \"hi\";\n    }\n}\n";

fn captured<'a>(
    ide: &Headless,
    doc: kumiko_types::DocumentId,
    text: &'a str,
) -> Vec<(&'a str, String)> {
    ide.core
        .highlights(doc, 0..text.len())
        .into_iter()
        .map(|h| (&text[h.range.clone()], h.capture.to_string()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn java_is_highlighted_and_folds_from_the_grammar_package() {
    build_grammar();
    let ide = Headless::start().await.unwrap();
    assert!(
        ide.core.syntax().languages().contains(&"java".to_owned()),
        "the java grammar package loads"
    );
    let path = ide.write("src/Hello.java", SOURCE).unwrap();
    let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
    let doc = ide.core.documents().open(&uri, None).await.unwrap();
    assert_eq!(ide.core.language_of(doc.id).as_deref(), Some("java"));

    let found = captured(&ide, doc.id, SOURCE);
    let has = |text: &str, capture: &str| {
        found
            .iter()
            .any(|(t, c)| *t == text && (c == capture || c.starts_with(&format!("{capture}."))))
    };
    assert!(has("public", "keyword"), "{found:?}");
    assert!(has("class", "keyword"));
    assert!(has("// greet", "comment"));
    assert!(has("\"hi\"", "string"));
    assert!(has("String", "type"));
    assert!(has("greet", "function"));

    // Class body, method block.
    let folds = ide.core.folds(doc.id);
    assert!(folds.contains(&(0, 5)), "{folds:?}");
    assert!(folds.contains(&(2, 4)), "{folds:?}");

    // An edit: the tree follows incrementally and highlights the new text.
    let edit = TextEdit {
        range: Range::new(Position::new(3, 19), Position::new(3, 23)),
        new_text: "\"привет\"".into(),
    };
    ide.core
        .documents()
        .apply(doc.id, doc.version, vec![edit])
        .unwrap();
    let text = ide
        .core
        .documents()
        .snapshot(doc.id)
        .unwrap()
        .text
        .to_string();
    let after = captured(&ide, doc.id, &text);
    assert!(
        after
            .iter()
            .any(|(t, c)| *t == "\"привет\"" && c == "string"),
        "{after:?}"
    );
}
