//! E15: the budgets of spec section 23 that E14 left unmeasured and the core can measure
//! without a window: plugin activation, a WASM plugin's idle memory, memory with ten open
//! files, the IDE's share of a completion, the core's latency during a storm of
//! `fs.changed`. The numbers count in release builds; debug builds only print. Run:
//! `cargo test --release --test headless e15_bench -- --ignored --nocapture --test-threads 1`.
//! With `KUMIKO_BENCH_OUT` naming a file, the numbers are written there as JSON.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kumiko_headless::kumiko_core::language::ServerStatus;
use kumiko_headless::kumiko_core::plugin_host::PluginStore;
use kumiko_headless::{Headless, eventually};
use kumiko_types::{CommandId, Position, Uri};
use serde_json::{Value, json};

use crate::fixtures;

/// Resident memory of this process in MB (Linux; elsewhere `None`).
fn rss_mb() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn percentile(samples: &mut [Duration], p: f64) -> Duration {
    samples.sort();
    let at = ((samples.len() as f64 - 1.0) * p).round() as usize;
    samples[at]
}

fn wasm_source(dir: &Path) -> PathBuf {
    let extra = ["--release", "--target", "wasm32-wasip2"];
    let component = fixtures::build("kumiko-echo-wasm", &extra, "kumiko_echo_wasm", ".wasm");
    let manifest = fixtures::workspace_root().join("sdk/examples/echo-wasm/plugin.toml");
    fixtures::plugin_source(
        dir,
        "bench-wasm",
        &manifest,
        |m| m,
        &[("plugin.wasm", &component)],
    )
}

/// `kumiko.echo` (a process plugin) with the fake language server next to it.
fn process_source(dir: &Path) -> PathBuf {
    let echo = fixtures::build("kumiko-echo", &[], "kumiko-echo", "");
    let server = fixtures::build("kumiko-fake-lsp", &[], "kumiko-fake-lsp", "");
    let manifest = fixtures::workspace_root().join("sdk/examples/echo/plugin.toml");
    let suffix = std::env::consts::EXE_SUFFIX;
    let echo_name = format!("bin/kumiko-echo{suffix}");
    let server_name = format!("bin/kumiko-fake-lsp{suffix}");
    let files = [
        (echo_name.as_str(), echo.as_path()),
        (server_name.as_str(), server.as_path()),
    ];
    fixtures::plugin_source(dir, "bench-process", &manifest, |m| m, &files)
}

async fn run(ide: &Headless, command: &str) -> Duration {
    let id = CommandId::new(command).unwrap();
    let t = Instant::now();
    ide.core.commands().execute(&id, vec![]).await.unwrap();
    t.elapsed()
}

/// Activation = the first command (starts the plugin) minus a later one (only the call).
async fn activation(ide: &Headless, command: &str) -> Duration {
    let first = run(ide, command).await;
    let mut calls: Vec<Duration> = Vec::new();
    for _ in 0..20 {
        calls.push(run(ide, command).await);
    }
    first.saturating_sub(percentile(&mut calls, 0.5))
}

async fn plugins(numbers: &mut serde_json::Map<String, Value>) {
    let ide = Headless::start().await.unwrap();
    let source = wasm_source(ide.home());

    // The first plugin also starts the engine (wasmtime, Cranelift): reported apart.
    let before = rss_mb();
    let plugin = ide.core.install_plugin(&source).await.unwrap();
    let cold = activation(&ide, "echo-wasm.count").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let with_engine = before.zip(rss_mb()).map(|(b, a)| a - b);
    // Installed again: the compiled component comes from the cache, and the difference is
    // what one more plugin instance costs (memory freed by the uninstall may be reused).
    ide.core.uninstall_plugin(&plugin).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = rss_mb();
    ide.core.install_plugin(&source).await.unwrap();
    let warm = activation(&ide, "echo-wasm.count").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let overhead = before.zip(rss_mb()).map(|(b, a)| a - b);
    ide.core.shutdown().await;

    let ide = Headless::start().await.unwrap();
    ide.core
        .install_plugin(&process_source(ide.home()))
        .await
        .unwrap();
    let process = activation(&ide, "echo.whoami").await;
    ide.core.shutdown().await;

    eprintln!(
        "budget: WASM activation cold {cold:?}, cached {warm:?}; process activation {process:?}; \
         WASM idle overhead {overhead:?} MB per plugin, {with_engine:?} MB with the engine"
    );
    numbers.insert("wasm_activation_cold_ms".into(), json!(ms(cold)));
    numbers.insert("wasm_activation_cached_ms".into(), json!(ms(warm)));
    numbers.insert("process_activation_ms".into(), json!(ms(process)));
    numbers.insert("wasm_idle_overhead_mb".into(), json!(overhead));
    numbers.insert(
        "wasm_first_plugin_with_engine_mb".into(),
        json!(with_engine),
    );
    if !cfg!(debug_assertions) {
        assert!(warm < Duration::from_millis(50), "WASM activation {warm:?}");
        assert!(
            process < Duration::from_millis(300),
            "process activation {process:?}"
        );
        if let Some(mb) = overhead {
            assert!(mb < 10.0, "WASM idle overhead {mb} MB");
        }
    }
}

/// The fake server answers at once, so the round trip is the IDE's share (and an upper
/// bound of it: the server's parsing and writing count too).
async fn completion(numbers: &mut serde_json::Map<String, Value>) {
    let ide = Headless::start_with(|options| {
        let source = process_source(options.dirs.data_dir());
        PluginStore::new(&options.dirs).install(&source).unwrap();
    })
    .await
    .unwrap();
    let workspace = ide.workspace().to_path_buf();
    ide.core.open_folder(&workspace).await.unwrap();
    let path = ide.write("src/a.fake", "hello world\nx.al\n").unwrap();
    let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
    ide.core.documents().open(&uri, None).await.unwrap();
    let language = ide.core.language();
    eventually("the server running", || {
        language
            .servers()
            .iter()
            .any(|(_, status)| *status == ServerStatus::Running)
    })
    .await
    .unwrap();
    let at = Position::new(1, 2);
    for _ in 0..20 {
        language.completion(&uri, at).await;
    }
    let mut samples = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        let items = language.completion(&uri, at).await;
        samples.push(t.elapsed());
        assert!(!items.is_empty());
    }
    let p50 = percentile(&mut samples, 0.5);
    let p95 = percentile(&mut samples, 0.95);
    ide.core.shutdown().await;
    eprintln!("budget: completion round trip with an instant server p50 {p50:?}, p95 {p95:?}");
    numbers.insert("completion_roundtrip_p50_ms".into(), json!(ms(p50)));
    numbers.insert("completion_roundtrip_p95_ms".into(), json!(ms(p95)));
    if !cfg!(debug_assertions) {
        assert!(p95 < Duration::from_millis(10), "completion {p95:?}");
    }
}

/// A project of 500 classes (spec section 23: «средний»), ten files open with highlighting.
async fn ten_open_files(numbers: &mut serde_json::Map<String, Value>) {
    fixtures::build_java_grammar();
    let ide = Headless::start().await.unwrap();
    for module in 0..10 {
        for class in 0..50 {
            let body: String = (0..40)
                .map(|m| {
                    format!("    public int method{m}(int x) {{ return x * {m} + {class}; }}\n")
                })
                .collect();
            ide.write(
                &format!("m{module}/src/main/java/m{module}/C{class}.java"),
                &format!("package m{module};\n\npublic class C{class} {{\n{body}}}\n"),
            )
            .unwrap();
        }
    }
    let workspace = ide.workspace().to_path_buf();
    ide.core.open_folder(&workspace).await.unwrap();
    for module in 0..10 {
        let path = workspace.join(format!("m{module}/src/main/java/m{module}/C0.java"));
        let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
        let doc = ide.core.documents().open(&uri, None).await.unwrap().id;
        let _ = ide.core.highlights(doc, 0..2_000);
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    let rss = rss_mb();
    ide.core.shutdown().await;
    eprintln!("budget: RSS of the core (no window) with 10 open files of 500 classes {rss:?} MB");
    numbers.insert("rss_mb_core_10_open_files".into(), json!(rss));
    if !cfg!(debug_assertions)
        && let Some(mb) = rss
    {
        assert!(mb < 300.0, "RSS {mb} MB");
    }
}

/// 10 000 files appear in `target/` while the core keeps answering what the UI asks for.
async fn fs_changed_storm(numbers: &mut serde_json::Map<String, Value>) {
    let ide = Headless::start().await.unwrap();
    ide.write("pom.xml", "<project/>").unwrap();
    let workspace = ide.workspace().to_path_buf();
    ide.core.open_folder(&workspace).await.unwrap();
    let path = ide.write("src/A.java", "class A {}\n").unwrap();
    let uri = Uri::from_file_path(kumiko_types::paths::canonicalize(&path).unwrap()).unwrap();
    let doc = ide.core.documents().open(&uri, None).await.unwrap().id;

    let target = workspace.join("target/classes");
    let writer = std::thread::spawn(move || {
        for i in 0..10_000 {
            let dir = target.join(format!("p{}", i / 100));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("C{i}.class")), [0u8; 64]).unwrap();
        }
    });
    let started = Instant::now();
    let mut worst = Duration::ZERO;
    let mut probes = 0;
    while !writer.is_finished() || started.elapsed() < Duration::from_secs(3) {
        let t = Instant::now();
        let _ = ide.core.file_index().find("a", 20);
        let _ = ide.core.documents().snapshot(doc).unwrap();
        let _ = ide.core.highlights(doc, 0..10);
        worst = worst.max(t.elapsed());
        probes += 1;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    writer.join().unwrap();
    ide.core.shutdown().await;
    eprintln!("budget: worst core latency during the fs.changed storm {worst:?} ({probes} probes)");
    numbers.insert("fs_changed_storm_worst_latency_ms".into(), json!(ms(worst)));
    if !cfg!(debug_assertions) {
        assert!(worst < Duration::from_millis(50), "storm {worst:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmarks; run with --release --ignored"]
async fn budgets_left_after_e14() {
    let mut numbers = serde_json::Map::new();
    plugins(&mut numbers).await;
    completion(&mut numbers).await;
    ten_open_files(&mut numbers).await;
    fs_changed_storm(&mut numbers).await;
    if let Some(out) = std::env::var_os("KUMIKO_BENCH_OUT") {
        std::fs::write(out, serde_json::to_string_pretty(&numbers).unwrap()).unwrap();
    }
}
