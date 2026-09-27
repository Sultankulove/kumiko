//! Editor tabs as data: one tab per file, a new tab opens next to the active one, closing a
//! tab activates its right neighbour (or the left one at the end).

use kumiko_types::Uri;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub uri: Uri,
    pub title: String,
}

#[derive(Debug, Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    active: Option<usize>,
}

impl Tabs {
    pub fn iter(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active?)
    }

    pub fn position(&self, uri: &Uri) -> Option<usize> {
        self.tabs.iter().position(|t| &t.uri == uri)
    }

    /// Activates the file's tab, opening it after the active tab if there is none.
    pub fn open(&mut self, uri: Uri, title: String) -> usize {
        let index = match self.position(&uri) {
            Some(index) => index,
            None => {
                let at = self.active.map_or(self.tabs.len(), |a| a + 1);
                self.tabs.insert(at, Tab { uri, title });
                at
            }
        };
        self.active = Some(index);
        index
    }

    pub fn activate(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = Some(index);
        }
    }

    /// Closes a tab; returns it.
    pub fn close(&mut self, index: usize) -> Option<Tab> {
        if index >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(index);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) if active == index => Some(index.min(self.tabs.len() - 1)),
            other => other,
        };
        Some(tab)
    }

    /// Moves to the next (`1`) or previous (`-1`) tab, wrapping around.
    pub fn cycle(&mut self, delta: isize) {
        let (Some(active), len) = (self.active, self.tabs.len()) else {
            return;
        };
        let len = len as isize;
        self.active = Some((active as isize + delta).rem_euclid(len) as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(name: &str) -> Uri {
        Uri::parse(format!("file:///w/{name}")).unwrap()
    }

    fn titles(tabs: &Tabs) -> Vec<&str> {
        tabs.iter().map(|t| t.title.as_str()).collect()
    }

    #[test]
    fn open_close_and_cycle() {
        let mut tabs = Tabs::default();
        assert_eq!(tabs.open(uri("a"), "a".into()), 0);
        assert_eq!(tabs.open(uri("b"), "b".into()), 1);
        tabs.activate(0);
        assert_eq!(tabs.open(uri("c"), "c".into()), 1, "next to the active tab");
        assert_eq!(titles(&tabs), ["a", "c", "b"]);
        assert_eq!(tabs.open(uri("a"), "a".into()), 0, "no duplicates");

        tabs.cycle(-1);
        assert_eq!(tabs.active().unwrap().title, "b");
        tabs.cycle(1);
        assert_eq!(tabs.active().unwrap().title, "a");

        tabs.activate(1);
        assert_eq!(tabs.close(1).unwrap().title, "c");
        assert_eq!(tabs.active().unwrap().title, "b", "right neighbour");
        tabs.close(1);
        assert_eq!(tabs.active().unwrap().title, "a", "left at the end");
        tabs.close(0);
        assert_eq!(tabs.active(), None);
        assert_eq!(tabs.close(0), None);
    }

    #[test]
    fn closing_before_the_active_tab_keeps_it_active() {
        let mut tabs = Tabs::default();
        for name in ["a", "b", "c"] {
            tabs.open(uri(name), name.into());
        }
        tabs.close(0);
        assert_eq!(tabs.active().unwrap().title, "c");
    }
}
