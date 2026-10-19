//! E10: tasks (spec, section 19). Plugins list them, the core runs them through the Process
//! service, streams the output into a channel of their own and reports start and finish.
//! `kumiko.echo` gives a task that needs nothing installed; `kumiko.java` lists the Maven
//! phases of a project (running Maven itself is in the manual scenario: it downloads).

use std::path::PathBuf;
use std::time::Duration;

use kumiko_headless::kumiko_core::plugin_host::PluginStore;
use kumiko_headless::kumiko_core::protocol::events::{TaskFinished, TaskStarted};
use kumiko_headless::kumiko_core::service_api::Event;
use kumiko_headless::{Headless, eventually, wait_for};

use crate::fixtures;

fn echo_source(dir: &std::path::Path) -> PathBuf {
    let binary = fixtures::build("kumiko-echo", &[], "kumiko-echo", "");
    let manifest = fixtures::workspace_root().join("sdk/examples/echo/plugin.toml");
    let name = format!("bin/kumiko-echo{}", std::env::consts::EXE_SUFFIX);
    fixtures::plugin_source(
        dir,
        "echo-tasks",
        &manifest,
        |m| m,
        &[(name.as_str(), &binary)],
    )
}

fn java_source(dir: &std::path::Path) -> PathBuf {
    let binary = fixtures::build("kumiko-java", &[], "kumiko-java", "");
    let root = fixtures::workspace_root().join("plugins/java");
    let entry = format!("bin/kumiko-java{}", std::env::consts::EXE_SUFFIX);
    let schema = root.join("schema/config.json");
    let files = [
        (entry.as_str(), binary.as_path()),
        ("schema/config.json", schema.as_path()),
    ];
    fixtures::plugin_source(dir, "java-tasks", &root.join("plugin.toml"), |m| m, &files)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasks_are_listed_run_and_reported() {
    let ide = Headless::start_with(|options| {
        let store = PluginStore::new(&options.dirs);
        store
            .install(&echo_source(options.dirs.data_dir()))
            .unwrap();
        store
            .install(&java_source(options.dirs.data_dir()))
            .unwrap();
    })
    .await
    .unwrap();
    ide.write("pom.xml", "<project/>").unwrap();
    let workspace = ide.workspace().to_path_buf();
    ide.core.open_folder(&workspace).await.unwrap();

    let tasks = ide.core.tasks().await;
    let labels: Vec<&str> = tasks.iter().map(|t| t.definition.label.as_str()).collect();
    assert!(labels.contains(&"echo: hello"), "{labels:?}");
    assert!(labels.contains(&"maven: test"), "{labels:?}");

    let mut events =
        ide.core
            .events()
            .subscribe(kumiko_headless::kumiko_core::events::EventFilter::topics([
                "task.started",
                "task.finished",
            ]));
    let hello = tasks
        .into_iter()
        .find(|t| t.definition.label == "echo: hello")
        .unwrap();
    let channel = hello.channel();
    let execution = ide.core.run_task(hello).await.unwrap();
    let is = |e: &Event| {
        e.decode::<TaskStarted>()
            .is_some_and(|s| s.unwrap().execution == execution)
    };
    wait_for(&mut events, "task.started", is).await.unwrap();
    let finished = wait_for(&mut events, "task.finished", |e| {
        e.decode::<TaskFinished>()
            .is_some_and(|f| f.unwrap().execution == execution)
    })
    .await
    .unwrap();
    let finished = finished.decode::<TaskFinished>().unwrap().unwrap();
    assert_eq!(finished.exit_code, Some(0));

    let output = ide.core.output();
    eventually("the output", || {
        let lines = output.lines(&channel);
        lines.iter().any(|l| l == "hello from a task") && lines.iter().any(|l| l == "done")
    })
    .await
    .unwrap();
    assert!(ide.core.running_tasks().is_empty());
    tokio::time::sleep(Duration::from_millis(10)).await;
}
