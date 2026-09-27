//! E7: the file index behind Quick Open follows the workspace: it lists the folder's files,
//! finds them fuzzily, respects `files.exclude` and picks up new files by itself.

use serde_json::json;

use kumiko_headless::kumiko_core::protocol::methods::config::ConfigTarget;
use kumiko_headless::{Headless, eventually};

fn found(ide: &Headless, query: &str) -> Vec<String> {
    ide.core
        .file_index()
        .find(query, 10)
        .into_iter()
        .map(|m| m.file.relative)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quick_open_index_follows_the_workspace() {
    let ide = Headless::start().await.unwrap();
    ide.write("src/main/App.txt", "app").unwrap();
    ide.write("src/test/AppTest.txt", "test").unwrap();
    ide.write("build/out/App.bin", "bin").unwrap();
    ide.core.open_folder(ide.workspace()).await.unwrap();

    // Indexed in the background after the folder opens.
    eventually("the folder is indexed", || ide.core.file_index().len() >= 3)
        .await
        .unwrap();
    assert_eq!(found(&ide, "apptest"), ["src/test/AppTest.txt"]);
    assert_eq!(found(&ide, "sma")[0], "src/main/App.txt");

    ide.core
        .config()
        .update("files.exclude", json!(["**/build"]), ConfigTarget::User)
        .unwrap();
    eventually("excluded files leave the index", || {
        !found(&ide, "").iter().any(|f| f.starts_with("build/"))
    })
    .await
    .unwrap();

    ide.write("src/main/Service.txt", "new").unwrap();
    eventually("a new file is indexed", || {
        found(&ide, "service") == ["src/main/Service.txt"]
    })
    .await
    .unwrap();
}
