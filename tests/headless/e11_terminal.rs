//! E11: terminals (spec, section 19, MVP criterion 12 without the window): a shell in a
//! pseudo-terminal, keys in, output out, its end reported.

use std::io::Read;
use std::time::Duration;

use kumiko_headless::kumiko_core::protocol::events::{TerminalClosed, TerminalOpened};
use kumiko_headless::kumiko_core::service_api::{ProcessSpec, PtySize};
use kumiko_headless::{Headless, wait_for};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shell_runs_in_a_terminal() {
    let ide = Headless::start().await.unwrap();
    let mut events =
        ide.core
            .events()
            .subscribe(kumiko_headless::kumiko_core::events::EventFilter::topics([
                "terminal.opened",
                "terminal.closed",
            ]));
    let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
    let spec = ProcessSpec {
        command: shell.into(),
        ..ProcessSpec::default()
    };
    let id = ide
        .core
        .open_terminal("Local", spec, PtySize { cols: 80, rows: 24 })
        .unwrap();
    wait_for(&mut events, "terminal.opened", |e| {
        e.decode::<TerminalOpened>()
            .is_some_and(|o| o.unwrap().terminal == id)
    })
    .await
    .unwrap();
    assert_eq!(ide.core.terminals(), [(id, "Local".to_owned())]);

    let terminal = ide.core.terminal(id).unwrap();
    let mut output = terminal.take_output().unwrap();
    assert!(terminal.take_output().is_none(), "one reader");
    let (tx, mut seen) = tokio::sync::mpsc::unbounded_channel::<String>();
    let writer = terminal.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        let mut all = String::new();
        while let Ok(n) = output.read(&mut buffer) {
            if n == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&buffer[..n]).into_owned();
            // ConPTY asks for the cursor position first; a terminal answers.
            if text.contains("\x1b[6n") {
                writer.write(b"\x1b[1;1R");
            }
            all.push_str(&text);
            if tx.send(all.clone()).is_err() {
                break;
            }
        }
    });

    terminal.write(b"echo hello-from-kumiko\r");
    let found = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(all) = seen.recv().await {
            // The echoed command line and the output: the text twice.
            if all.matches("hello-from-kumiko").count() >= 2 {
                return true;
            }
        }
        false
    })
    .await;
    assert_eq!(found, Ok(true));

    terminal.write(b"exit\r");
    let closed = wait_for(&mut events, "terminal.closed", |e| {
        e.decode::<TerminalClosed>()
            .is_some_and(|c| c.unwrap().terminal == id)
    })
    .await
    .unwrap();
    assert_eq!(
        closed
            .decode::<TerminalClosed>()
            .unwrap()
            .unwrap()
            .exit_code,
        Some(0)
    );
    assert!(ide.core.terminals().is_empty());
}

/// `kumiko.terminal` (WASM) decides what a session runs; the core opens it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_terminal_plugin_opens_sessions() {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use kumiko_headless::kumiko_core::plugin_host::PluginStore;
    use kumiko_types::CommandId;

    use crate::fixtures;

    static COMPONENT: OnceLock<PathBuf> = OnceLock::new();
    let component = COMPONENT.get_or_init(|| {
        let extra = ["--release", "--target", "wasm32-wasip2"];
        fixtures::build("kumiko-terminal", &extra, "kumiko_terminal", ".wasm")
    });
    let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
    let ide = Headless::start_with(|options| {
        let root = fixtures::workspace_root().join("plugins/terminal");
        let schema = root.join("schema/config.json");
        let files = [
            ("plugin.wasm", component.as_path()),
            ("schema/config.json", schema.as_path()),
        ];
        let source = fixtures::plugin_source(
            options.dirs.data_dir(),
            "terminal-src",
            &root.join("plugin.toml"),
            |m| m,
            &files,
        );
        PluginStore::new(&options.dirs).install(&source).unwrap();
        let settings = options.dirs.user_settings_file();
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(settings, format!("[terminal]\nshell = \"{shell}\"\n")).unwrap();
    })
    .await
    .unwrap();
    let mut events =
        ide.core
            .events()
            .subscribe(kumiko_headless::kumiko_core::events::EventFilter::topics([
                "terminal.opened",
            ]));
    let id = CommandId::new("terminal.new").unwrap();
    let answer = ide.core.commands().execute(&id, vec![]).await.unwrap();
    let opened = wait_for(&mut events, "terminal.opened", |e| {
        e.decode::<TerminalOpened>().is_some()
    })
    .await
    .unwrap();
    let opened = opened.decode::<TerminalOpened>().unwrap().unwrap();
    assert_eq!(opened.title, "Local");
    assert_eq!(answer["terminal"], serde_json::json!(opened.terminal.0));
    ide.core.close_terminal(opened.terminal);
}
