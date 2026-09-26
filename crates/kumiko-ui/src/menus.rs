//! The main menu, built from commands (spec, section 20): each menu lists the workbench's own
//! commands that are registered, then plugin items from `contributes.menus` whose location is
//! `menu/<name>` (e.g. `menu/tools`). Unregistered commands are left out; disabled ones are
//! shown greyed.

use kumiko_types::CommandId;

/// Top-level menus: l10n key of the title, location name, built-in commands (`None` is a
/// separator).
const MAIN_MENUS: &[(&str, &str, &[Option<&str>])] = &[
    (
        "menu.file",
        "file",
        &[
            Some("file.save"),
            Some("file.saveAll"),
            None,
            Some("workbench.quit"),
        ],
    ),
    ("menu.edit", "edit", &[Some("edit.undo"), Some("edit.redo")]),
    (
        "menu.view",
        "view",
        &[
            Some("workbench.toggleProject"),
            Some("workbench.toggleProblems"),
            Some("workbench.toggleOutput"),
        ],
    ),
    (
        "menu.navigate",
        "navigate",
        &[Some("workbench.quickOpen"), Some("workbench.showCommands")],
    ),
    ("menu.tools", "tools", &[]),
    ("menu.help", "help", &[]),
];

#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub command: CommandId,
    pub title: String,
    pub keys: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub title: String,
    /// `None` is a separator.
    pub items: Vec<Option<MenuItem>>,
}

/// What building the menu needs to know about commands.
pub trait CommandLookup {
    /// The localized title, `None` when the command is not registered.
    fn title(&self, command: &CommandId) -> Option<String>;
    fn enabled(&self, command: &CommandId) -> bool;
    fn keys(&self, command: &CommandId) -> Option<String>;
}

/// The main menu. `contributions` are `(location, command)` pairs whose `when` holds.
pub fn main_menu(
    lookup: &dyn CommandLookup,
    localize: &dyn Fn(&str) -> String,
    contributions: &[(String, CommandId)],
) -> Vec<Menu> {
    MAIN_MENUS
        .iter()
        .map(|(title, location, builtin)| {
            let item = |command: CommandId| {
                Some(MenuItem {
                    title: lookup.title(&command)?,
                    enabled: lookup.enabled(&command),
                    keys: lookup.keys(&command),
                    command,
                })
            };
            let mut items: Vec<Option<MenuItem>> = Vec::new();
            for entry in builtin.iter() {
                match entry {
                    None => items.push(None),
                    Some(id) => {
                        if let Some(item) = CommandId::new(*id).ok().and_then(item) {
                            items.push(Some(item));
                        }
                    }
                }
            }
            let wanted = format!("menu/{location}");
            let plugin_items: Vec<MenuItem> = contributions
                .iter()
                .filter(|(at, _)| *at == wanted)
                .filter_map(|(_, command)| item(command.clone()))
                .collect();
            if !plugin_items.is_empty() {
                items.push(None);
                items.extend(plugin_items.into_iter().map(Some));
            }
            Menu {
                title: localize(title),
                items: tidy(items),
            }
        })
        .collect()
}

/// Drops separators at the ends and doubled ones (left over from missing commands).
fn tidy(items: Vec<Option<MenuItem>>) -> Vec<Option<MenuItem>> {
    let mut out: Vec<Option<MenuItem>> = Vec::with_capacity(items.len());
    for item in items {
        if item.is_none() && out.last().is_none_or(Option::is_none) {
            continue;
        }
        out.push(item);
    }
    if out.last().is_some_and(Option::is_none) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl CommandLookup for Fake {
        fn title(&self, command: &CommandId) -> Option<String> {
            let registered = [
                "workbench.quit",
                "workbench.toggleProject",
                "workbench.toggleProblems",
                "demo.run",
            ];
            registered
                .contains(&command.as_str())
                .then(|| format!("<{command}>"))
        }

        fn enabled(&self, command: &CommandId) -> bool {
            command.as_str() != "workbench.toggleProblems"
        }

        fn keys(&self, command: &CommandId) -> Option<String> {
            (command.as_str() == "workbench.toggleProject").then(|| "Alt+1".to_owned())
        }
    }

    fn titles(menu: &Menu) -> Vec<String> {
        menu.items
            .iter()
            .map(|i| i.as_ref().map_or("--".to_owned(), |i| i.title.clone()))
            .collect()
    }

    #[test]
    fn menus_list_registered_commands_and_plugin_items() {
        let demo = CommandId::new("demo.run").unwrap();
        let contributions = vec![
            ("menu/tools".to_owned(), demo.clone()),
            ("editor/context".to_owned(), demo),
        ];
        let menus = main_menu(&Fake, &|key| format!("[{key}]"), &contributions);
        assert_eq!(menus.len(), 6);
        // file.save and file.saveAll are not registered: the separator goes too.
        assert_eq!(menus[0].title, "[menu.file]");
        assert_eq!(titles(&menus[0]), ["<workbench.quit>"]);
        assert!(menus[1].items.is_empty());
        assert_eq!(
            titles(&menus[2]),
            ["<workbench.toggleProject>", "<workbench.toggleProblems>"]
        );
        let project = menus[2].items[0].as_ref().unwrap();
        assert_eq!(project.keys.as_deref(), Some("Alt+1"));
        assert!(project.enabled);
        assert!(!menus[2].items[1].as_ref().unwrap().enabled);
        assert_eq!(titles(&menus[4]), ["<demo.run>"]);
    }
}
