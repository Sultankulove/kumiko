//! E15: MVP criteria 2, 7–9 and 18 on spring-petclinic with the real JDT LS (spec v1.1,
//! section 9). Needs a petclinic checkout (`KUMIKO_PETCLINIC`) whose Maven dependencies are
//! already downloaded (`./mvnw test-compile` once) and JDT LS (`KUMIKO_JDTLS`); run with
//! `--ignored`. The checkout is copied, so JDT LS never writes into it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use kumiko_headless::Headless;
use kumiko_headless::kumiko_core::events::{Delivery, Subscriber};
use kumiko_headless::kumiko_core::language::Severity;
use kumiko_headless::kumiko_core::plugin_host::PluginStore;
use kumiko_headless::kumiko_core::protocol::events::TaskFinished;
use kumiko_headless::kumiko_core::protocol::methods::task::TaskExecutionId;
use kumiko_types::{Position, Range, TextEdit, Uri};

use crate::e10_java::{plugin_source, until};
use crate::fixtures;

const CONTROLLER: &str =
    "src/main/java/org/springframework/samples/petclinic/owner/OwnerController.java";
const REPOSITORY: &str = "OwnerRepository.java";

/// Copies the project without build output and VCS data.
fn copy_project(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if matches!(name.to_str(), Some(".git" | "target" | "build" | ".gradle")) {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_project(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Where `needle` starts in `text`, plus `offset` characters (the file is ASCII).
fn position_of(text: &str, needle: &str, offset: u32) -> Position {
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not found"));
    let line = text[..at].matches('\n').count();
    let column = at - text[..at].rfind('\n').map_or(0, |n| n + 1);
    Position::new(line as u32, column as u32 + offset)
}

/// A copy of the checkout in a fresh IDE with `kumiko.java` installed.
async fn open_petclinic() -> (Headless, PathBuf) {
    let install = std::env::var("KUMIKO_JDTLS").expect("KUMIKO_JDTLS");
    let checkout = std::env::var("KUMIKO_PETCLINIC").expect("KUMIKO_PETCLINIC");
    fixtures::build_java_grammar();
    let settings = format!("[java.jdtls]\npath = {:?}\n", install.replace('\\', "/"));
    let ide = Headless::start_with(move |options| {
        let source = plugin_source(options.dirs.data_dir());
        PluginStore::new(&options.dirs).install(&source).unwrap();
        let file = options.dirs.user_settings_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, settings).unwrap();
    })
    .await
    .unwrap();
    let workspace = ide.workspace().to_path_buf();
    copy_project(Path::new(&checkout), &workspace);
    // Criterion 2: the Maven project opens.
    ide.core.open_folder(&workspace).await.unwrap();
    (ide, workspace)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs JDT LS and spring-petclinic: set KUMIKO_JDTLS and KUMIKO_PETCLINIC"]
async fn spring_petclinic_with_jdt_ls() {
    let (ide, workspace) = open_petclinic().await;
    let path = kumiko_types::paths::canonicalize(workspace.join(CONTROLLER)).unwrap();
    let uri = Uri::from_file_path(&path).unwrap();
    let doc = ide.core.documents().open(&uri, None).await.unwrap().id;
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\r\n", "\n");
    let language = ide.core.language();
    let context = ide.core.commands().context();
    let slow = Duration::from_secs(600);
    until("java.serverReady", slow, async || {
        context.get("java.serverReady") == Some(serde_json::json!(true))
    })
    .await;

    // Criterion 7, and the Maven classpath is resolved: `count` comes from Spring Data.
    let after_owners = position_of(
        &text,
        "this.owners.save(owner);",
        "this.owners.".len() as u32,
    );
    let mut labels = Vec::new();
    until("completion from Spring Data", slow, async || {
        labels = language
            .completion(&uri, after_owners)
            .await
            .into_iter()
            .map(|i| i.label)
            .collect();
        labels.iter().any(|l| l.starts_with("findById"))
            && labels.iter().any(|l| l.starts_with("count"))
    })
    .await;

    // The controller compiles against the project's dependencies.
    let errors: Vec<_> = language
        .diagnostics(&uri)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");

    // Criterion 9: `findById` is declared in OwnerRepository.
    let call = position_of(
        &text,
        "this.owners.findById(ownerId);",
        "this.owners.".len() as u32 + 1,
    );
    let found = language.definition(&uri, call).await;
    assert!(
        found
            .first()
            .is_some_and(|l| l.uri.as_str().ends_with(REPOSITORY)),
        "{found:?}"
    );

    // Criteria 8 and 18: without its import `Optional` is an error the quick fix repairs.
    let import = "import java.util.Optional;\n";
    let start = position_of(&text, import, 0);
    let removed = Range::new(start, Position::new(start.line + 1, 0));
    let documents = ide.core.documents();
    let version = documents.snapshot(doc).unwrap().version;
    documents
        .apply(
            doc,
            version,
            vec![TextEdit {
                range: removed,
                new_text: String::new(),
            }],
        )
        .unwrap();
    let mut unresolved = None;
    until("the Optional diagnostic", slow, async || {
        unresolved = language
            .diagnostics(&uri)
            .into_iter()
            .find(|d| d.severity == Severity::Error && d.message.contains("Optional"));
        unresolved.is_some()
    })
    .await;
    let range = unresolved.unwrap().range;
    let actions = language.code_actions(&uri, range).await;
    let fix = actions
        .iter()
        .find(|a| a.title.contains("Import") && a.title.contains("java.util"))
        .unwrap_or_else(|| panic!("{:?}", actions.iter().map(|a| &a.title).collect::<Vec<_>>()));
    language.apply_code_action(fix).await.unwrap();
    let edited = documents.snapshot(doc).unwrap().text.to_string();
    assert!(edited.contains("import java.util.Optional;"), "{edited}");

    // JDT LS keeps its metadata out of the project.
    for name in [".project", ".classpath", ".settings"] {
        assert!(!workspace.join(name).exists(), "{name} was created");
    }
    ide.core.shutdown().await;
}

/// The exit code of `execution`; Maven takes longer than `kumiko_headless::wait_for` waits.
async fn wait_for_finish(events: &mut Subscriber, execution: &TaskExecutionId) -> Option<i32> {
    loop {
        match events.recv().await {
            Some(Delivery::Event(event)) => {
                if let Some(Ok(finished)) = event.decode::<TaskFinished>()
                    && finished.execution == *execution
                {
                    return finished.exit_code;
                }
            }
            Some(_) => continue,
            None => panic!("the event stream ended"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs JDT LS and spring-petclinic: set KUMIKO_JDTLS and KUMIKO_PETCLINIC"]
async fn spring_petclinic_maven_test_task() {
    let (ide, _workspace) = open_petclinic().await;
    let task = ide
        .core
        .tasks()
        .await
        .into_iter()
        .find(|t| t.definition.label == "maven: test")
        .expect("maven: test");
    let channel = task.channel();
    let mut events = ide.subscribe(&["task.finished"]);
    let execution = ide.core.run_task(task).await.unwrap();

    // Criterion 11: Maven runs the project's tests with the project's JDK.
    let finished = tokio::time::timeout(
        Duration::from_secs(900),
        wait_for_finish(&mut events, &execution),
    )
    .await
    .expect("maven: test did not finish");
    let lines = ide.core.output().lines(&channel);
    assert_eq!(finished, Some(0), "{}", lines.join("\n"));
    assert!(lines.iter().any(|l| l.contains("BUILD SUCCESS")));
    ide.core.shutdown().await;
}
