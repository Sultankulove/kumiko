//! E13: kits (MVP criterion 17): the built-in `minimal` and `java`, applying a kit installs,
//! enables and disables plugins without rebuilding anything, its settings are a layer under
//! the user's, applying it again changes nothing, export describes the state.

use std::path::PathBuf;
use std::sync::OnceLock;

use kumiko_headless::Headless;
use kumiko_types::PluginId;

use crate::fixtures;

fn echo_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let binary = fixtures::build("kumiko-echo", &[], "kumiko-echo", "");
        let manifest = fixtures::workspace_root().join("sdk/examples/echo/plugin.toml");
        let name = format!("bin/kumiko-echo{}", std::env::consts::EXE_SUFFIX);
        let parent = std::env::temp_dir().join(format!("kumiko-kits-{}", std::process::id()));
        std::fs::create_dir_all(&parent).unwrap();
        fixtures::plugin_source(
            &parent,
            "kumiko.echo",
            &manifest,
            |m| m,
            &[(name.as_str(), &binary)],
        )
    })
}

fn echo() -> PluginId {
    PluginId::new("kumiko.echo").unwrap()
}

fn echo_state(ide: &Headless) -> Option<bool> {
    ide.core.plugins().get(&echo()).map(|e| e.is_ready())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kits_install_enable_disable_and_layer_settings() {
    let ide = Headless::start().await.unwrap();
    let core = &ide.core;

    // The built-in kits.
    let ids: Vec<String> = core.kits().into_iter().map(|k| k.id).collect();
    assert!(ids.contains(&"kumiko.kit.minimal".to_owned()), "{ids:?}");
    assert!(ids.contains(&"kumiko.kit.java".to_owned()), "{ids:?}");
    let java = core.load_kit("java").unwrap();
    assert_eq!(java.plugins.len(), 3);
    let plan = core.plan_kit(&java);
    assert_eq!(
        plan.missing.len(),
        3,
        "no source here: reported, not failed"
    );
    assert!(plan.is_empty());

    // A kit of our own with a source: installs the plugin, sets a setting.
    let kit_file = ide.home().join("echo.kit.toml");
    std::fs::write(
        &kit_file,
        format!(
            "[kit]\nid = \"test.kit.echo\"\nname = \"Echo\"\nversion = \"0.1.0\"\n\n\
             [plugins]\n\"kumiko.echo\" = \"^0.1\"\n\n\
             [sources]\n\"kumiko.echo\" = {{ path = {:?} }}\n\n\
             [settings]\n\"editor.fontSize\" = 20\n",
            echo_dir().to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    let kit = core.load_kit(kit_file.to_str().unwrap()).unwrap();
    assert_eq!(echo_state(&ide), None);
    let plan = core.apply_kit(&kit).await.unwrap();
    assert_eq!(plan.install.len(), 1);
    assert_eq!(echo_state(&ide), Some(true));
    assert_eq!(
        core.config().get("editor.fontSize", None),
        Some(serde_json::json!(20))
    );
    assert_eq!(core.active_kit().as_deref(), Some("test.kit.echo"));

    // Idempotent: again, nothing to do.
    assert!(core.plan_kit(&kit).is_empty());
    assert!(core.apply_kit(&kit).await.unwrap().is_empty());

    // `minimal` is exclusive: the plugin is disabled, not removed. Switch back: enabled again.
    let minimal = core.load_kit("minimal").unwrap();
    let plan = core.apply_kit(&minimal).await.unwrap();
    assert_eq!(plan.disable, [echo()]);
    assert_eq!(echo_state(&ide), Some(false));
    assert_eq!(
        core.plugins()
            .get(&echo())
            .map(|e| e.manifest().id().clone()),
        Some(echo())
    );
    let plan = core.apply_kit(&kit).await.unwrap();
    assert_eq!(plan.enable, [echo()]);
    assert_eq!(echo_state(&ide), Some(true));

    // The kit's settings are below the user's.
    core.config()
        .update(
            "editor.fontSize",
            serde_json::json!(11),
            kumiko_headless::kumiko_core::protocol::methods::config::ConfigTarget::User,
        )
        .unwrap();
    assert_eq!(
        core.config().get("editor.fontSize", None),
        Some(serde_json::json!(11))
    );

    // Export: the state as a kit that reads back and plans to nothing.
    let exported = core.export_kit("test.kit.snapshot", "Snapshot");
    assert!(exported.contains("\"kumiko.echo\""), "{exported}");
    let snapshot = ide.home().join("snapshot.kit.toml");
    std::fs::write(&snapshot, &exported).unwrap();
    let again = core.load_kit(snapshot.to_str().unwrap()).unwrap();
    assert!(core.plan_kit(&again).is_empty());

    // Mistakes name the file.
    let broken = ide.home().join("broken.kit.toml");
    std::fs::write(&broken, "[kit]\nid = \"x\"\n").unwrap();
    let error = core
        .load_kit(broken.to_str().unwrap())
        .unwrap_err()
        .to_string();
    assert!(error.contains("broken.kit.toml"), "{error}");
    assert!(
        core.load_kit("no-such-kit")
            .unwrap_err()
            .to_string()
            .contains("known:")
    );
}
