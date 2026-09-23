use std::sync::Mutex;

use serde_json::json;

use super::*;

#[derive(Default)]
struct Collect(Mutex<Vec<Vec<String>>>);

impl EventSink for Collect {
    fn publish(&self, event: Event) {
        if let Some(Ok(changed)) = event.decode::<ConfigChanged>() {
            self.0.lock().unwrap().push(changed.keys);
        }
    }
}

impl Collect {
    fn take(&self) -> Vec<Vec<String>> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    user: PathBuf,
    workspace: PathBuf,
    events: Arc<Collect>,
    config: ConfigService,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("config/settings.toml");
    let workspace = dir.path().join("work/.kumiko/settings.toml");
    let events = Arc::new(Collect::default());
    let config = ConfigService::new(events.clone());
    config
        .register_schema(
            "core",
            &json!({"properties": {
                "editor.fontSize": {"type": "integer", "default": 14, "minimum": 6},
                "editor.tabSize": {"type": "integer", "default": 4},
                "files.eol": {"enum": ["lf", "crlf"], "default": "lf"},
                "files.exclude": {"type": "object", "default": {}},
            }}),
        )
        .unwrap();
    config.set_user_file(Some(user.clone()));
    config.set_workspace_file(Some(workspace.clone()));
    events.take();
    Fixture {
        _dir: dir,
        user,
        workspace,
        events,
        config,
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn layers_override_in_order() {
    let f = fixture();
    assert_eq!(f.config.get("editor.fontSize", None), Some(json!(14)));

    f.config
        .set_kit(Map::from_iter([("editor.fontSize".into(), json!(13))]));
    assert_eq!(f.config.get("editor.fontSize", None), Some(json!(13)));

    write(
        &f.user,
        "[editor]\nfontSize = 15\ntabSize = 2\n[language.rust]\n\"editor.tabSize\" = 8\n",
    );
    write(
        &f.workspace,
        "\"editor.fontSize\" = 16\n[language.rust.editor]\nfontSize = 18\n",
    );
    f.config.reload();
    assert_eq!(f.config.get("editor.fontSize", None), Some(json!(16)));
    assert_eq!(f.config.get("editor.tabSize", None), Some(json!(2)));
    assert_eq!(
        f.config.get("editor.fontSize", Some("rust")),
        Some(json!(18))
    );
    assert_eq!(f.config.get("editor.tabSize", Some("rust")), Some(json!(8)));
    assert_eq!(
        f.config.get("editor.tabSize", Some("python")),
        Some(json!(2))
    );
    assert_eq!(f.config.get("unknown.key", None), None);
}

#[test]
fn invalid_values_fall_back_and_are_reported() {
    let f = fixture();
    write(
        &f.user,
        "\"editor.fontSize\" = 2\n\"files.eol\" = \"cr\"\n\"editor.tabSize\" = 3\n",
    );
    write(&f.workspace, "\"editor.tabSize\" = \"wide\"\n");
    f.config.reload();
    assert_eq!(
        f.config.get("editor.fontSize", None),
        Some(json!(14)),
        "below minimum"
    );
    assert_eq!(
        f.config.get("files.eol", None),
        Some(json!("lf")),
        "not in enum"
    );
    assert_eq!(
        f.config.get("editor.tabSize", None),
        Some(json!(3)),
        "user wins over bad workspace"
    );
    let keys: Vec<_> = f
        .config
        .problems()
        .into_iter()
        .filter_map(|p| p.key)
        .collect();
    assert_eq!(keys, ["editor.fontSize", "files.eol", "editor.tabSize"]);
}

#[test]
fn broken_files_keep_the_previous_values() {
    let f = fixture();
    write(&f.user, "\"editor.fontSize\" = 20\n");
    f.config.reload();
    write(&f.user, "\"editor.fontSize\" = \n");
    f.config.reload();
    assert_eq!(f.config.get("editor.fontSize", None), Some(json!(20)));
    let problems = f.config.problems();
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0]
            .message
            .contains("keeping the previous settings")
    );
}

#[test]
fn object_settings_are_not_flattened() {
    let f = fixture();
    write(&f.user, "[files.exclude]\n\"**/out\" = true\n");
    f.config.reload();
    assert_eq!(
        f.config.get("files.exclude", None),
        Some(json!({"**/out": true}))
    );
    assert_eq!(
        Value::Object(f.config.section("files", None)),
        json!({"eol": "lf", "exclude": {"**/out": true}})
    );
}

#[test]
fn reload_publishes_only_changed_keys() {
    let f = fixture();
    write(&f.user, "\"editor.fontSize\" = 15\n");
    assert_eq!(f.config.reload(), ["editor.fontSize"]);
    assert_eq!(f.config.reload(), Vec::<String>::new());
    write(
        &f.user,
        "\"editor.fontSize\" = 15\n[language.rust]\n\"editor.tabSize\" = 2\n",
    );
    assert_eq!(f.config.reload(), ["editor.tabSize"]);
    assert_eq!(
        f.events.take(),
        [vec!["editor.fontSize"], vec!["editor.tabSize"]]
    );
}

#[test]
fn update_writes_the_target_file_and_keeps_comments() {
    let f = fixture();
    write(&f.user, "# my settings\n[editor]\nfontSize = 12 # small\n");
    f.config.reload();
    f.events.take();

    f.config
        .update("editor.fontSize", json!(17), ConfigTarget::User)
        .unwrap();
    let text = std::fs::read_to_string(&f.user).unwrap();
    assert!(
        text.contains("# my settings") && text.contains("\"editor.fontSize\" = 17"),
        "{text}"
    );
    assert_eq!(f.config.get("editor.fontSize", None), Some(json!(17)));
    assert_eq!(f.events.take(), [vec!["editor.fontSize"]]);

    f.config
        .update("editor.tabSize", json!(2), ConfigTarget::Workspace)
        .unwrap();
    assert!(
        f.workspace.exists(),
        "the workspace .kumiko directory is created"
    );
    f.config
        .update("editor.tabSize", Value::Null, ConfigTarget::Workspace)
        .unwrap();
    assert_eq!(f.config.get("editor.tabSize", None), Some(json!(4)));

    assert!(matches!(
        f.config
            .update("editor.fontSize", json!("big"), ConfigTarget::User),
        Err(ConfigError::Invalid { .. })
    ));
    f.config.set_workspace_file(None);
    assert_eq!(
        f.config
            .update("editor.tabSize", json!(2), ConfigTarget::Workspace),
        Err(ConfigError::NoFile(ConfigTarget::Workspace))
    );
}

#[test]
fn schemas_are_owned() {
    let f = fixture();
    let plugin = json!({"properties": {"sample.level": {"type": "integer", "default": 1}}});
    f.config
        .register_schema("org.example.sample", &plugin)
        .unwrap();
    assert_eq!(f.config.get("sample.level", None), Some(json!(1)));
    assert_eq!(f.events.take(), [vec!["sample.level"]]);
    assert!(matches!(
        f.config.register_schema("other", &plugin),
        Err(ConfigError::DuplicateKey { .. })
    ));
    f.config.unregister_schema("org.example.sample");
    assert_eq!(f.config.get("sample.level", None), None);
}

#[test]
fn keymap_files_load_into_their_source() {
    let f = fixture();
    let path = f.user.with_file_name("keymap.toml");
    write(
        &path,
        "[[binding]]\nkey = \"ctrl+s\"\ncommand = \"file.saveAll\"\n[[binding]]\nkey = \"???\"\n",
    );
    f.config.load_keymap_file(KeymapSource::User, &path);
    assert_eq!(f.config.keymap().bindings().len(), 1);
    assert_eq!(f.config.problems().len(), 1);
    std::fs::remove_file(&path).unwrap();
    f.config.load_keymap_file(KeymapSource::User, &path);
    assert!(f.config.keymap().bindings().is_empty());
    assert!(f.config.problems().is_empty());
}
