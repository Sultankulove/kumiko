//! The project tree as data: workspace folders at the top, directories loaded lazily when
//! expanded, keyboard navigation as in JetBrains IDEs (arrows move, right expands or enters,
//! left collapses or goes to the parent). The view only draws [`FileTree::rows`].

use std::collections::{HashMap, HashSet};

use kumiko_types::Uri;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub uri: Uri,
    pub name: String,
    pub dir: bool,
}

/// A visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub uri: Uri,
    pub name: String,
    pub depth: usize,
    pub dir: bool,
    pub expanded: bool,
}

#[derive(Debug, Default)]
pub struct FileTree {
    roots: Vec<Entry>,
    children: HashMap<Uri, Vec<Entry>>,
    parents: HashMap<Uri, Uri>,
    expanded: HashSet<Uri>,
    selected: Option<Uri>,
}

impl FileTree {
    /// Workspace folders; new ones start expanded. Returns folders whose children to load.
    pub fn set_roots(&mut self, roots: Vec<Entry>) -> Vec<Uri> {
        let known: HashSet<Uri> = self.roots.iter().map(|r| r.uri.clone()).collect();
        let mut load = Vec::new();
        for root in &roots {
            if !known.contains(&root.uri) {
                self.expanded.insert(root.uri.clone());
                load.push(root.uri.clone());
            }
        }
        self.roots = roots;
        if self.selected.is_none() {
            self.selected = self.roots.first().map(|r| r.uri.clone());
        }
        load
    }

    /// The loaded contents of `dir`: directories first, then files, by name ignoring case.
    pub fn set_children(&mut self, dir: &Uri, mut entries: Vec<Entry>) {
        entries.sort_by(|a, b| {
            b.dir
                .cmp(&a.dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });
        for entry in &entries {
            self.parents.insert(entry.uri.clone(), dir.clone());
        }
        self.children.insert(dir.clone(), entries);
    }

    pub fn is_loaded(&self, dir: &Uri) -> bool {
        self.children.contains_key(dir)
    }

    /// Directories whose contents are shown and must be reloaded when they change.
    pub fn loaded_dirs(&self) -> impl Iterator<Item = &Uri> {
        self.children.keys()
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for root in &self.roots {
            self.push_rows(root, 0, &mut rows);
        }
        rows
    }

    fn push_rows(&self, entry: &Entry, depth: usize, rows: &mut Vec<Row>) {
        let expanded = entry.dir && self.expanded.contains(&entry.uri);
        rows.push(Row {
            uri: entry.uri.clone(),
            name: entry.name.clone(),
            depth,
            dir: entry.dir,
            expanded,
        });
        if expanded && let Some(children) = self.children.get(&entry.uri) {
            for child in children {
                self.push_rows(child, depth + 1, rows);
            }
        }
    }

    pub fn selected(&self) -> Option<&Uri> {
        self.selected.as_ref()
    }

    pub fn select(&mut self, uri: Uri) {
        self.selected = Some(uri);
    }

    /// Expands or collapses a directory; returns it when its contents must be loaded.
    pub fn toggle(&mut self, dir: &Uri) -> Option<Uri> {
        if self.expanded.remove(dir) {
            return None;
        }
        self.expanded.insert(dir.clone());
        (!self.is_loaded(dir)).then(|| dir.clone())
    }

    /// Moves the selection by `delta` visible rows.
    pub fn move_selection(&mut self, delta: isize) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|s| rows.iter().position(|r| &r.uri == s));
        let next = match current {
            None => 0,
            Some(i) => i.saturating_add_signed(delta).min(rows.len() - 1),
        };
        self.selected = Some(rows[next].uri.clone());
    }

    /// Right arrow: expands a collapsed directory (returning it if it must be loaded), or
    /// moves into an expanded one.
    pub fn right(&mut self) -> Option<Uri> {
        let row = self.selected_row()?;
        if !row.dir {
            return None;
        }
        if !row.expanded {
            return self.toggle(&row.uri);
        }
        self.move_selection(1);
        None
    }

    /// Left arrow: collapses an expanded directory, or moves to the parent.
    pub fn left(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.expanded {
            self.expanded.remove(&row.uri);
        } else if let Some(parent) = self.parents.get(&row.uri) {
            self.selected = Some(parent.clone());
        }
    }

    pub fn selected_row(&self) -> Option<Row> {
        let selected = self.selected.as_ref()?;
        self.rows().into_iter().find(|r| &r.uri == selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(path: &str) -> Uri {
        Uri::parse(format!("file:///w{path}")).unwrap()
    }

    fn entry(path: &str, dir: bool) -> Entry {
        Entry {
            uri: uri(path),
            name: path.rsplit('/').next().unwrap().to_owned(),
            dir,
        }
    }

    fn names(tree: &FileTree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    fn sample() -> FileTree {
        let mut tree = FileTree::default();
        let load = tree.set_roots(vec![Entry {
            uri: uri(""),
            name: "w".into(),
            dir: true,
        }]);
        assert_eq!(load, [uri("")]);
        tree.set_children(
            &uri(""),
            vec![
                entry("/b.txt", false),
                entry("/src", true),
                entry("/A.txt", false),
                entry("/docs", true),
            ],
        );
        tree
    }

    #[test]
    fn directories_come_first_and_load_lazily() {
        let mut tree = sample();
        assert_eq!(names(&tree), ["w", "  docs", "  src", "  A.txt", "  b.txt"]);
        assert_eq!(tree.toggle(&uri("/src")), Some(uri("/src")));
        // Expanded but not loaded yet: nothing below it.
        assert_eq!(tree.rows().len(), 5);
        tree.set_children(&uri("/src"), vec![entry("/src/main.txt", false)]);
        assert_eq!(names(&tree)[3], "    main.txt");
        assert_eq!(tree.toggle(&uri("/src")), None);
        assert_eq!(tree.rows().len(), 5);
        assert_eq!(tree.toggle(&uri("/src")), None, "already loaded");
    }

    #[test]
    fn arrows_navigate_like_jetbrains() {
        let mut tree = sample();
        assert_eq!(tree.selected(), Some(&uri("")));
        tree.move_selection(1);
        tree.move_selection(1);
        assert_eq!(tree.selected(), Some(&uri("/src")));
        assert_eq!(tree.right(), Some(uri("/src")), "expand and load");
        tree.set_children(&uri("/src"), vec![entry("/src/main.txt", false)]);
        assert_eq!(tree.right(), None, "enter the directory");
        assert_eq!(tree.selected(), Some(&uri("/src/main.txt")));
        tree.left();
        assert_eq!(tree.selected(), Some(&uri("/src")), "file: go to parent");
        tree.left();
        assert!(
            !tree.selected_row().unwrap().expanded,
            "directory: collapse"
        );
        tree.move_selection(100);
        assert_eq!(tree.selected(), Some(&uri("/b.txt")));
        tree.move_selection(-100);
        assert_eq!(tree.selected(), Some(&uri("")));
    }
}
