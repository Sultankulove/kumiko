//! Selections of an editor view: one or more ranges (multiple cursors), each with a fixed
//! anchor and a moving head, in char offsets. Selections belong to the view, not to the
//! document (spec, section 12); edits made elsewhere move them through [`Selection::map`].
//!
//! The model follows Helix's (anchor/head ranges, merged when they overlap, positions mapped
//! through a change set with an association); the code is ours.

/// One replacement of a transaction: chars `start..end` of the text before the transaction
/// become `text`. A transaction's changes do not overlap and refer to the same old text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl Change {
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self {
            start: at,
            end: at,
            text: text.into(),
        }
    }

    pub fn delete(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            text: String::new(),
        }
    }

    fn new_len(&self) -> usize {
        self.text.chars().count()
    }
}

/// Which side of inserted text a position at the insertion point ends up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assoc {
    Before,
    After,
}

/// Maps a position of the old text into the new one. `changes` are sorted by start.
pub fn map_position(position: usize, changes: &[Change], assoc: Assoc) -> usize {
    let mut delta: isize = 0;
    for change in changes {
        let start = (change.start as isize + delta) as usize;
        let len = change.new_len();
        if position < change.start {
            break;
        }
        let empty = change.start == change.end;
        if position == change.start && (!empty || assoc == Assoc::Before) {
            return start;
        }
        if position < change.end || (empty && position == change.start) {
            // Inside the replaced text, or at an insertion point with `After`.
            return match assoc {
                Assoc::Before => start,
                Assoc::After => start + len,
            };
        }
        if position == change.end {
            return start + len;
        }
        delta += len as isize - (change.end - change.start) as isize;
    }
    (position as isize + delta) as usize
}

/// A selected range: `anchor` stays when extending, `head` is the caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub anchor: usize,
    pub head: usize,
    /// The display column vertical moves aim for, so the caret does not drift on short lines.
    pub goal: Option<usize>,
}

impl Range {
    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal: None,
        }
    }

    /// A caret without selected text.
    pub fn point(at: usize) -> Self {
        Self::new(at, at)
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Moves the caret; with `extend` the anchor stays, otherwise the range collapses.
    pub fn moved_to(&self, head: usize, extend: bool) -> Self {
        Self::new(if extend { self.anchor } else { head }, head)
    }

    /// Whether two ranges must merge: they overlap, or carets coincide.
    fn overlaps(&self, other: &Range) -> bool {
        self.start() < other.end() && other.start() < self.end()
            || self.start() == other.start() && self.end() == other.end()
            || self.is_empty() && other.start() < self.head && self.head < other.end()
            || other.is_empty() && self.start() < other.head && other.head < self.end()
    }
}

/// One or more ranges, sorted and never overlapping; one of them is the primary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    ranges: Vec<Range>,
    primary: usize,
}

impl Selection {
    pub fn point(at: usize) -> Self {
        Self::single(Range::point(at))
    }

    pub fn single(range: Range) -> Self {
        Self {
            ranges: vec![range],
            primary: 0,
        }
    }

    /// Normalizes the ranges: sorted by start, overlapping ones merged.
    pub fn new(ranges: Vec<Range>, primary: usize) -> Self {
        assert!(!ranges.is_empty(), "a selection has at least one range");
        let primary = primary.min(ranges.len() - 1);
        let mut indexed: Vec<(usize, Range)> = ranges.into_iter().enumerate().collect();
        indexed.sort_by_key(|(_, r)| (r.start(), r.end()));
        let mut merged: Vec<Range> = Vec::with_capacity(indexed.len());
        let mut new_primary = 0;
        for (index, range) in indexed {
            match merged.last_mut() {
                Some(last) if last.overlaps(&range) => {
                    let (start, end) =
                        (last.start().min(range.start()), last.end().max(range.end()));
                    // The merged range keeps the direction of the later one.
                    *last = if range.head < range.anchor {
                        Range::new(end, start)
                    } else {
                        Range::new(start, end)
                    };
                }
                _ => merged.push(range),
            }
            if index == primary {
                new_primary = merged.len() - 1;
            }
        }
        Self {
            ranges: merged,
            primary: new_primary,
        }
    }

    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Applies `f` to every range and normalizes the result.
    pub fn transform(&self, mut f: impl FnMut(Range) -> Range) -> Self {
        Self::new(self.ranges.iter().map(|r| f(*r)).collect(), self.primary)
    }

    /// Adds a range and makes it the primary one.
    pub fn with(&self, range: Range) -> Self {
        let mut ranges = self.ranges.clone();
        ranges.push(range);
        let primary = ranges.len() - 1;
        Self::new(ranges, primary)
    }

    /// Keeps only the primary range (Escape with several carets).
    pub fn only_primary(&self) -> Self {
        Self::single(self.primary())
    }

    /// Moves the ranges through a transaction made elsewhere; carets at an insertion point
    /// stay in front of the new text, so text typed by another view pushes them on.
    pub fn map(&self, changes: &[Change]) -> Self {
        self.transform(|r| Range {
            anchor: map_position(r.anchor, changes, Assoc::Before),
            head: map_position(r.head, changes, Assoc::Before),
            goal: None,
        })
    }

    /// Carets right after the text a transaction put in place of each range: the selection
    /// after typing, pasting or deleting.
    pub fn after(&self, changes: &[Change]) -> Self {
        self.transform(|r| Range::point(map_position(r.end(), changes, Assoc::After)))
    }

    /// Clamps every position to the document length (after it shrank).
    pub fn clamp(&self, len: usize) -> Self {
        self.transform(|r| Range {
            anchor: r.anchor.min(len),
            head: r.head.min(len),
            goal: r.goal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_map_through_changes() {
        // "hello world": replace "hello" (0..5) with "hi", insert "!" at 11.
        let changes = [
            Change {
                start: 0,
                end: 5,
                text: "hi".into(),
            },
            Change::insert(11, "!"),
        ];
        assert_eq!(
            map_position(0, &changes, Assoc::After),
            0,
            "start of a replacement"
        );
        assert_eq!(
            map_position(3, &changes, Assoc::Before),
            0,
            "inside: to the start"
        );
        assert_eq!(
            map_position(3, &changes, Assoc::After),
            2,
            "inside: past the new text"
        );
        assert_eq!(
            map_position(5, &changes, Assoc::Before),
            2,
            "end of a replacement"
        );
        assert_eq!(
            map_position(8, &changes, Assoc::Before),
            5,
            "between changes"
        );
        assert_eq!(
            map_position(11, &changes, Assoc::Before),
            8,
            "at an insertion"
        );
        assert_eq!(map_position(11, &changes, Assoc::After), 9);
        assert_eq!(map_position(4, &[], Assoc::After), 4);
    }

    #[test]
    fn overlapping_ranges_merge_and_the_primary_follows() {
        let selection = Selection::new(
            vec![
                Range::new(10, 14),
                Range::point(2),
                Range::new(12, 8),
                Range::point(2),
            ],
            2,
        );
        assert_eq!(selection.ranges(), [Range::point(2), Range::new(8, 14)]);
        assert_eq!(selection.primary_index(), 1);
        // Touching ranges stay apart; a caret strictly inside a range merges into it.
        let touching = Selection::new(vec![Range::new(0, 3), Range::new(3, 5)], 0);
        assert_eq!(touching.len(), 2);
        let inside = Selection::new(vec![Range::new(0, 6), Range::point(3)], 1);
        assert_eq!((inside.len(), inside.primary()), (1, Range::new(0, 6)));
    }

    #[test]
    fn typing_at_every_caret_and_edits_from_elsewhere() {
        // Carets at 1 and 5; each types "xy".
        let selection = Selection::new(vec![Range::point(1), Range::point(5)], 0);
        let changes = [Change::insert(1, "xy"), Change::insert(5, "xy")];
        let after = selection.after(&changes);
        assert_eq!(after.ranges(), [Range::point(3), Range::point(9)]);
        // Another view's caret at 5 stays in front of text inserted there.
        let other = Selection::point(5).map(&changes);
        assert_eq!(other.primary(), Range::point(7));
        // A selection over deleted text collapses to where it was.
        let gone = Selection::single(Range::new(2, 4)).map(&[Change::delete(1, 6)]);
        assert_eq!(gone.primary(), Range::point(1));
    }

    #[test]
    fn adding_a_caret_makes_it_primary() {
        let selection = Selection::point(4)
            .with(Range::point(9))
            .with(Range::point(1));
        assert_eq!(selection.len(), 3);
        assert_eq!(selection.primary(), Range::point(1));
        assert_eq!(selection.only_primary(), Selection::point(1));
        assert_eq!(
            selection.clamp(3).ranges(),
            [Range::point(1), Range::point(3)]
        );
    }
}
