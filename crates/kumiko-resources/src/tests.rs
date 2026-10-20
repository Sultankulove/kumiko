use std::path::PathBuf;

use super::*;

fn builtin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources")
}

fn package(root: &Path, dir: &str, text: &str) {
    let dir = root.join(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("resource.toml"), text).unwrap();
}

#[test]
fn shipped_resources_are_valid() {
    let registry = ResourceRegistry::new();
    let problems = registry.scan(&[builtin()]);
    assert!(problems.is_empty(), "{problems:#?}");

    let themes: Vec<_> = registry
        .list(ResourceKind::Theme)
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(
        themes,
        [
            "kumiko.theme.darcula",
            "kumiko.theme.high-contrast",
            "kumiko.theme.hinoki-dark",
            "kumiko.theme.hinoki-light"
        ]
    );
    assert_eq!(registry.list(ResourceKind::L10n).len(), 2);
    assert_eq!(registry.list(ResourceKind::Keymap).len(), 1);
    for tool in ["project", "commit", "problems", "output", "terminal"] {
        let icon = registry.icon(&format!("toolwindow.{tool}"), None).unwrap();
        assert!(icon.is_file(), "{}", icon.display());
    }

    // Every label a shipped manifest uses is translated in every shipped locale.
    for package in [
        "kumiko.theme.darcula",
        "kumiko.theme.hinoki-dark",
        "kumiko.keymap.default",
    ] {
        let name = registry.get(package).unwrap().name.unwrap();
        for locale in ["en", "ru"] {
            assert_ne!(registry.label(&name, locale), name, "{package} in {locale}");
        }
    }
}

#[test]
fn themes_resolve_tokens() {
    let registry = ResourceRegistry::new();
    registry.scan(&[builtin()]);
    let dark = registry.theme("kumiko.theme.hinoki-dark").unwrap();
    assert_eq!(dark.base, ThemeBase::Dark);
    assert_eq!(dark.color("accent"), Color::parse("#2A9D8F"));
    assert_eq!(
        dark.color("no.such.token"),
        None,
        "missing tokens are not errors"
    );
    let keyword = dark.syntax("keyword.control.flow").unwrap();
    assert!(keyword.bold);
    assert_eq!(keyword.fg, Color::parse("#E76F51"));
    assert!(dark.syntax("comment").unwrap().italic);
    assert!(registry.theme("kumiko.l10n.en").is_none(), "not a theme");
}

#[test]
fn themes_extend_others_and_user_packages_override() {
    let user = tempfile::tempdir().unwrap();
    package(
        user.path(),
        "my-dark",
        r##"
[resource]
id = "user.theme.my-dark"
kind = "theme"
[theme]
base = "dark"
extends = "kumiko.theme.hinoki-dark"
[colors]
accent = "#FF0000"
broken = "red"
"##,
    );
    package(
        user.path(),
        "ru-override",
        "[resource]\nid = \"kumiko.l10n.ru\"\nkind = \"l10n\"\n[l10n]\nlocale = \"ru\"\n[strings]\n\"command.file.save\" = \"Записать\"\n",
    );
    let registry = ResourceRegistry::new();
    let problems = registry.scan(&[builtin(), user.path().to_path_buf()]);
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].message.contains("broken"));

    let mine = registry.theme("user.theme.my-dark").unwrap();
    assert_eq!(mine.color("accent"), Color::parse("#FF0000"));
    assert_eq!(
        mine.color("editor.background"),
        Color::parse("#1E1E24"),
        "inherited"
    );
    assert_eq!(registry.localize("command.file.save", "ru"), "Записать");
}

#[test]
fn localization_falls_back() {
    let registry = ResourceRegistry::new();
    registry.scan(&[builtin()]);
    assert_eq!(registry.localize("command.file.save", "ru-RU"), "Сохранить");
    assert_eq!(
        registry.localize("command.file.save", "ru_RU.UTF-8"),
        "Сохранить"
    );
    assert_eq!(registry.localize("command.file.save", "de"), "Save");
    assert_eq!(registry.localize("no.such.key", "ru"), "no.such.key");
    assert_eq!(registry.label("%command.edit.undo%", "ru"), "Отменить");
    assert_eq!(registry.label("Plain title", "ru"), "Plain title");
}

#[test]
fn broken_packages_are_reported_not_fatal() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), "a", "not toml [");
    package(root.path(), "b", "[resource]\nkind = \"theme\"\n");
    package(
        root.path(),
        "c",
        "[resource]\nid = \"x\"\nkind = \"sound\"\n",
    );
    package(
        root.path(),
        "group/d",
        "[resource]\nid = \"t\"\nkind = \"theme\"\n[theme]\nbase = \"sepia\"\n",
    );
    package(
        root.path(),
        "e",
        "[resource]\nid = \"l\"\nkind = \"l10n\"\n",
    );
    let registry = ResourceRegistry::new();
    let problems = registry.scan(&[root.path().to_path_buf()]);
    assert_eq!(problems.len(), 6, "{problems:#?}");
    assert!(registry.get("x").is_none());
}

#[test]
fn colors_parse() {
    assert_eq!(
        Color::parse("#01020304"),
        Some(Color {
            r: 1,
            g: 2,
            b: 3,
            a: 4
        })
    );
    assert_eq!(Color::parse("#fff"), None);
    assert_eq!(Color::parse("123456"), None);
    assert_eq!(Color::parse("#12345G"), None);
}

#[test]
fn icons_come_from_the_preferred_pack_then_any() {
    let root = tempfile::tempdir().unwrap();
    for (dir, id, tokens) in [
        ("a", "test.icons.a", ["one", "two"]),
        ("b", "test.icons.b", ["one", "three"]),
    ] {
        let mut text = format!("[resource]\nid = \"{id}\"\nkind = \"icons\"\n[icons]\n");
        for token in tokens {
            text.push_str(&format!("\"{token}\" = \"{token}.svg\"\n"));
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
            std::fs::write(root.path().join(dir).join(format!("{token}.svg")), "<svg/>").unwrap();
        }
        package(root.path(), dir, &text);
    }
    package(
        root.path(),
        "broken",
        "[resource]\nid = \"test.icons.broken\"\nkind = \"icons\"\n[icons]\nx = \"missing.svg\"\n",
    );
    let registry = ResourceRegistry::new();
    let problems = registry.scan(&[root.path().to_path_buf()]);
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].message.contains("missing.svg"));

    let from = |token: &str, preferred| {
        let path = registry.icon(token, preferred)?;
        Some(path.parent()?.file_name()?.to_string_lossy().into_owned())
    };
    assert_eq!(from("one", Some("test.icons.b")).as_deref(), Some("b"));
    assert_eq!(from("one", None).as_deref(), Some("a"), "first by id");
    assert_eq!(
        from("two", Some("test.icons.b")).as_deref(),
        Some("a"),
        "missing in b"
    );
    assert_eq!(from("nothing", None), None);
}
