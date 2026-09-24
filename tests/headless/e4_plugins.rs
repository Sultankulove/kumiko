//! E4: installed plugins are resolved at startup and their contributions are registered before
//! any plugin code runs.

use std::path::Path;

use serde_json::json;

use kumiko_headless::Headless;
use kumiko_headless::kumiko_core::commands::Owner;
use kumiko_headless::kumiko_core::config::keymap::{KeySequence, Resolution};
use kumiko_headless::kumiko_core::plugin_host::PluginStatus;
use kumiko_types::{CommandId, PluginId};

/// Writes `plugins/<id>/<version>/` with a manifest and extra files, as `plugin install` would.
fn plugin(plugins: &Path, id: &str, manifest: &str, files: &[(&str, &str)]) {
    let dir = plugins.join(id).join("1.0.0");
    for (name, content) in [("plugin.toml", manifest)].iter().chain(files) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

fn header(id: &str, api: &str) -> String {
    format!(
        "[plugin]\nid = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\napi = \"{api}\"\nkind = \"process\"\nentry = \"bin/run\"\n"
    )
}

fn command(id: &str) -> CommandId {
    CommandId::new(id).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contributions_are_registered_at_startup() {
    let ide = Headless::start_with(|options| {
        let plugins = options.dirs.plugins_dir();
        plugin(
            &plugins,
            "test.echo",
            &format!(
                "{}{}",
                header("test.echo", "^0.1"),
                r#"
[[contributes.commands]]
id = "echo.say"
title = "Say"

[[contributes.commands]]
id = "echo.inWorkspace"
title = "Say in workspace"
when = "echo.ready"

[[contributes.keybindings]]
key = "ctrl+alt+e"
command = "echo.say"

[contributes.configuration]
schema = "schema/config.json"
"#
            ),
            &[
                ("bin/run", "#!/bin/sh\n"),
                (
                    "schema/config.json",
                    r#"{ "properties": { "echo.greeting": { "type": "string", "default": "hi" } } }"#,
                ),
            ],
        );
        plugin(
            &plugins,
            "test.future",
            &format!(
                "{}[[contributes.commands]]\nid = \"future.run\"\ntitle = \"Run\"\n",
                header("test.future", "^9.0")
            ),
            &[("bin/run", "")],
        );
        plugin(
            &plugins,
            "test.rogue",
            &format!("{}[contributes.configuration]\nschema = \"config.json\"\n", header("test.rogue", "^0.1")),
            &[
                ("bin/run", ""),
                (
                    "config.json",
                    r#"{ "properties": { "editor.tabSize": { "type": "integer", "default": 8 } } }"#,
                ),
            ],
        );
    })
    .await
    .unwrap();
    let core = &ide.core;

    // Statuses.
    let status = |id: &str| {
        core.plugins()
            .get(&PluginId::new(id).unwrap())
            .unwrap()
            .status
            .clone()
    };
    assert_eq!(status("test.echo"), PluginStatus::Ready);
    assert_eq!(status("test.rogue"), PluginStatus::Ready);
    assert!(matches!(
        status("test.future"),
        PluginStatus::Incompatible(_)
    ));

    // Commands of ready plugins are registered with their owner; incompatible ones are not.
    let say = core
        .commands()
        .list()
        .into_iter()
        .find(|c| c.id == command("echo.say"))
        .unwrap();
    assert_eq!(
        say.owner,
        Owner::Plugin(PluginId::new("test.echo").unwrap())
    );
    assert!(core.commands().is_enabled(&command("echo.say")));
    assert!(!core.commands().contains(&command("future.run")));

    // `when` is evaluated by the core over context keys.
    assert!(!core.commands().is_enabled(&command("echo.inWorkspace")));
    core.commands().context().set("echo.ready", json!(true));
    assert!(core.commands().is_enabled(&command("echo.inWorkspace")));
    assert_eq!(
        core.commands().context().get("plugin.test.echo.running"),
        Some(json!(false))
    );

    // Without the plugin bridge the command cannot run yet, and says why.
    let error = core
        .commands()
        .execute(&command("echo.say"), vec![])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("test.echo is not running"),
        "{error}"
    );

    // Keybindings go to the plugin layer of the keymap.
    let pressed = KeySequence::parse("ctrl+alt+e").unwrap();
    assert!(matches!(
        core.config().keymap().resolve(&pressed, core.commands().context()),
        Resolution::Command(id) if id == command("echo.say")
    ));

    // Settings: own namespace is registered, a foreign key is refused.
    assert_eq!(core.config().get("echo.greeting", None), Some(json!("hi")));
    assert_eq!(core.config().get("editor.tabSize", None), Some(json!(4)));
}
