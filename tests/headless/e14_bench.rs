//! E14 budgets of spec section 23 that the core can measure without a window (the numbers
//! count in release builds; debug builds only print): Quick Open on 50 000 files, the first
//! results of a project-wide search, a storm of `fs.changed`. Run:
//! `cargo test --release --test headless e14_ -- --ignored --nocapture`.
//! With `KUMIKO_BENCH_OUT` naming a file, the numbers are written there as JSON.

use std::time::{Duration, Instant};

use kumiko_headless::kumiko_core::search::text::TextQuery;
use kumiko_headless::{Headless, eventually};
use serde_json::json;

/// 50 000 files in 500 folders, each a few lines of text.
fn make_project(root: &std::path::Path) {
    for dir in 0..500 {
        let folder = root.join(format!("module{dir:03}/src/main/java/pkg{dir}"));
        std::fs::create_dir_all(&folder).unwrap();
        for file in 0..100 {
            std::fs::write(
                folder.join(format!("Class{file:03}Service.java")),
                format!("package pkg{dir};\npublic class Class{file:03}Service {{\n    // line {file}\n}}\n"),
            )
            .unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "builds a project of 50 000 files; run with --ignored"]
async fn budgets_on_fifty_thousand_files() {
    let ide = Headless::start().await.unwrap();
    make_project(ide.workspace());
    let started = Instant::now();
    ide.core.open_folder(ide.workspace()).await.unwrap();
    eventually("indexed", || ide.core.file_index().len() >= 50_000)
        .await
        .unwrap();
    let indexing = started.elapsed();

    // Quick Open: a result per typed character.
    let mut per_char = Vec::new();
    let query = "module250service";
    for end in 1..=query.len() {
        let t = Instant::now();
        let found = ide.core.file_index().find(&query[..end], 50);
        per_char.push(t.elapsed());
        assert!(!found.is_empty());
    }
    per_char.sort();
    let quick_open_max = *per_char.last().unwrap();

    // Find in Files: until the first result and until the end of a rare word.
    let regex_query = TextQuery {
        text: "Class042Service".into(),
        ..TextQuery::default()
    };
    let t = Instant::now();
    let mut found = ide.core.search_text(&regex_query, 1000).unwrap();
    found.recv().await.unwrap();
    let first_result = t.elapsed();
    let mut count = 1;
    while found.recv().await.is_some() {
        count += 1;
    }
    let whole_search = t.elapsed();

    eprintln!(
        "budget: indexing {indexing:?}, Quick Open worst per char {quick_open_max:?}, \
         search first result {first_result:?}, whole search {whole_search:?} ({count} matches)"
    );
    if let Some(out) = std::env::var_os("KUMIKO_BENCH_OUT") {
        let numbers = json!({
            "files": 50_000,
            "indexing_ms": indexing.as_millis(),
            "quick_open_worst_ms": quick_open_max.as_secs_f64() * 1000.0,
            "search_first_result_ms": first_result.as_secs_f64() * 1000.0,
            "search_whole_ms": whole_search.as_secs_f64() * 1000.0,
        });
        std::fs::write(out, serde_json::to_string_pretty(&numbers).unwrap()).unwrap();
    }
    if !cfg!(debug_assertions) {
        assert!(quick_open_max < Duration::from_millis(30), "Quick Open {quick_open_max:?}");
        assert!(first_result < Duration::from_millis(200), "search {first_result:?}");
    }
}
