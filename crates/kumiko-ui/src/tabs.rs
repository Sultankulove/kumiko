//! Editor tabs as data: one tab per file, a new tab opens next to the active one, closing a
//! tab activates its right neighbour (or the left one at the end). [`Split`] holds the editor
//! groups of a split editor area.

use kumiko_types::Uri;
use serde::{Deserialize, Serialize};

use crate::layout::Rect;

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

/// A group as its files and the index of its active tab.
pub type GroupFiles = (Vec<Uri>, Option<usize>);

/// How two editor groups share the editor area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Orientation {
    /// Side by side (Split Right).
    Right,
    /// One above the other (Split Down).
    Down,
}

/// Editor groups: one, or two after a split (the MVP limit). One group is active: commands
/// and newly opened files go there.
#[derive(Debug)]
pub struct Split {
    groups: Vec<Tabs>,
    active: usize,
    orientation: Orientation,
}

impl Default for Split {
    fn default() -> Self {
        Self {
            groups: vec![Tabs::default()],
            active: 0,
            orientation: Orientation::Right,
        }
    }
}

impl Split {
    pub fn groups(&self) -> &[Tabs] {
        &self.groups
    }

    pub fn active_group(&self) -> usize {
        self.active
    }

    pub fn focus_group(&mut self, index: usize) {
        if index < self.groups.len() {
            self.active = index;
        }
    }

    /// The active group's tabs.
    pub fn tabs(&self) -> &Tabs {
        &self.groups[self.active]
    }

    pub fn tabs_mut(&mut self) -> &mut Tabs {
        &mut self.groups[self.active]
    }

    /// Splits the editor area: the active file opens in the other group too, which becomes
    /// active. Splitting again only changes the orientation and moves the file over.
    pub fn split(&mut self, orientation: Orientation) {
        self.orientation = orientation;
        let Some(tab) = self.tabs().active().cloned() else {
            return;
        };
        if self.groups.len() == 1 {
            self.groups.push(Tabs::default());
        }
        self.active = 1 - self.active;
        self.tabs_mut().open(tab.uri, tab.title);
    }

    /// Joins the groups: the other group's tabs move into the active one.
    pub fn unsplit(&mut self) {
        if self.groups.len() < 2 {
            return;
        }
        let other = self.groups.remove(1 - self.active);
        self.active = 0;
        let keep = self.tabs().active_index();
        for tab in other.iter() {
            if self.tabs().position(&tab.uri).is_none() {
                // `open` puts a tab after the active one: activate the last to append.
                let tabs = self.tabs_mut();
                if let Some(last) = tabs.len().checked_sub(1) {
                    tabs.activate(last);
                }
                tabs.open(tab.uri.clone(), tab.title.clone());
            }
        }
        if let Some(index) = keep {
            self.tabs_mut().activate(index);
        }
    }

    /// Closes the active group's active tab; an emptied second group goes away.
    pub fn close_active(&mut self) -> Option<Tab> {
        let index = self.tabs().active_index()?;
        let tab = self.tabs_mut().close(index);
        if self.groups.len() > 1 && self.tabs().len() == 0 {
            self.groups.remove(self.active);
            self.active = 0;
        }
        tab
    }

    /// The groups, the active group and the orientation.
    pub fn to_state(&self) -> (Vec<GroupFiles>, usize, Orientation) {
        let groups = self
            .groups
            .iter()
            .map(|g| (g.iter().map(|t| t.uri.clone()).collect(), g.active_index()))
            .collect();
        (groups, self.active, self.orientation)
    }

    /// Rebuilds the groups (at most two; empty ones are dropped).
    pub fn restore(
        groups: Vec<(Vec<Tab>, Option<usize>)>,
        active: usize,
        orientation: Orientation,
    ) -> Self {
        let mut restored: Vec<Tabs> = groups
            .into_iter()
            .filter(|(tabs, _)| !tabs.is_empty())
            .take(2)
            .map(|(tabs, active)| {
                let mut group = Tabs::default();
                for tab in tabs {
                    group.open(tab.uri, tab.title);
                }
                if let Some(index) = active {
                    group.activate(index);
                }
                group
            })
            .collect();
        if restored.is_empty() {
            restored.push(Tabs::default());
        }
        let active = active.min(restored.len() - 1);
        Self {
            groups: restored,
            active,
            orientation,
        }
    }

    /// Closes the file's tabs everywhere (it could not be opened); empty groups go away.
    pub fn remove_file(&mut self, uri: &Uri) {
        for group in &mut self.groups {
            if let Some(index) = group.position(uri) {
                group.close(index);
            }
        }
        if self.groups.len() > 1 {
            self.groups.retain(|g| g.len() > 0);
            if self.groups.is_empty() {
                self.groups.push(Tabs::default());
            }
            self.active = self.active.min(self.groups.len() - 1);
        }
    }

    /// Whether any group shows the file.
    pub fn holds(&self, uri: &Uri) -> bool {
        self.groups.iter().any(|g| g.position(uri).is_some())
    }

    /// Where each group is drawn inside the editor area (a 1 px divider between).
    pub fn rects(&self, area: Rect) -> Vec<Rect> {
        if self.groups.len() < 2 {
            return vec![area];
        }
        match self.orientation {
            Orientation::Right => {
                let w = ((area.w - 1.0) / 2.0).max(0.0).floor();
                vec![
                    Rect::new(area.x, area.y, w, area.h),
                    Rect::new(
                        area.x + w + 1.0,
                        area.y,
                        (area.w - w - 1.0).max(0.0),
                        area.h,
                    ),
                ]
            }
            Orientation::Down => {
                let h = ((area.h - 1.0) / 2.0).max(0.0).floor();
                vec![
                    Rect::new(area.x, area.y, area.w, h),
                    Rect::new(
                        area.x,
                        area.y + h + 1.0,
                        area.w,
                        (area.h - h - 1.0).max(0.0),
                    ),
                ]
            }
        }
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

    #[test]
    fn split_duplicates_the_file_and_closing_merges_back() {
        let mut split = Split::default();
        split.split(Orientation::Right);
        assert_eq!(split.groups().len(), 1, "nothing to split without a file");
        split.tabs_mut().open(uri("a"), "a".into());
        split.tabs_mut().open(uri("b"), "b".into());
        split.split(Orientation::Right);
        assert_eq!((split.groups().len(), split.active_group()), (2, 1));
        assert_eq!(titles(split.tabs()), ["b"]);
        assert!(split.holds(&uri("a")) && split.holds(&uri("b")));

        split.tabs_mut().open(uri("c"), "c".into());
        split.focus_group(0);
        split.unsplit();
        assert_eq!(split.groups().len(), 1);
        assert_eq!(titles(split.tabs()), ["a", "b", "c"]);
        assert_eq!(
            split.tabs().active().unwrap().title,
            "b",
            "the active tab stays"
        );

        split.split(Orientation::Down);
        assert_eq!(split.close_active().unwrap().title, "b");
        assert_eq!(split.groups().len(), 1, "an emptied group goes away");
        assert!(split.holds(&uri("b")), "still open in the first group");
    }

    #[test]
    fn groups_share_the_area() {
        let mut split = Split::default();
        let area = Rect::new(0.0, 0.0, 101.0, 51.0);
        assert_eq!(split.rects(area), [area]);
        split.tabs_mut().open(uri("a"), "a".into());
        split.split(Orientation::Right);
        assert_eq!(
            split.rects(area),
            [
                Rect::new(0.0, 0.0, 50.0, 51.0),
                Rect::new(51.0, 0.0, 50.0, 51.0)
            ]
        );
        split.split(Orientation::Down);
        assert_eq!(
            split.rects(area),
            [
                Rect::new(0.0, 0.0, 101.0, 25.0),
                Rect::new(0.0, 26.0, 101.0, 25.0)
            ]
        );
    }

    #[test]
    fn split_state_round_trips_and_missing_files_go() {
        let tab = |n: &str| Tab {
            uri: uri(n),
            title: n.into(),
        };
        let mut split = Split::restore(
            vec![
                (vec![tab("a"), tab("b")], Some(0)),
                (vec![], None),
                (vec![tab("c")], Some(0)),
            ],
            1,
            Orientation::Down,
        );
        assert_eq!(split.groups().len(), 2, "empty groups are dropped");
        assert_eq!(titles(&split.groups()[0]), ["a", "b"]);
        assert_eq!(split.groups()[0].active().unwrap().title, "a");
        let (groups, active, orientation) = split.to_state();
        assert_eq!(groups[1], (vec![uri("c")], Some(0)));
        assert_eq!((active, orientation), (1, Orientation::Down));

        split.remove_file(&uri("c"));
        assert_eq!(split.groups().len(), 1);
        assert_eq!(split.active_group(), 0);
        assert!(!split.holds(&uri("c")));
        let empty = Split::restore(Vec::new(), 3, Orientation::Right);
        assert_eq!((empty.groups().len(), empty.active_group()), (1, 0));
    }
}
