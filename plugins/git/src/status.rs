//! `git status --porcelain=v2 -z --branch`: the branch and the changed files.

use serde_json::{Value, json};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<FileStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    /// Path relative to the repository, `/`-separated.
    pub path: String,
    /// The path before a rename.
    pub original: Option<String>,
    /// Index (staged) and work tree letters of `git status`: `M`, `A`, `D`, `R`, `.` …
    pub index: char,
    pub worktree: char,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Changed,
    Renamed,
    Conflict,
    Untracked,
}

impl FileStatus {
    /// What happened to the file, for colours and badges: added, deleted, modified, …
    pub fn change(&self) -> &'static str {
        match (self.kind, self.index, self.worktree) {
            (Kind::Untracked, _, _) => "untracked",
            (Kind::Conflict, _, _) => "conflict",
            (Kind::Renamed, _, _) => "renamed",
            (_, 'A', _) => "added",
            (_, 'D', _) | (_, _, 'D') => "deleted",
            _ => "modified",
        }
    }

    pub fn staged(&self) -> bool {
        !matches!(self.kind, Kind::Untracked) && self.index != '.'
    }

    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "original": self.original,
            "change": self.change(),
            "staged": self.staged(),
        })
    }
}

impl Status {
    pub fn to_json(&self) -> Value {
        json!({
            "branch": self.branch,
            "upstream": self.upstream,
            "ahead": self.ahead,
            "behind": self.behind,
            "files": self.files.iter().map(FileStatus::to_json).collect::<Vec<_>>(),
        })
    }
}

/// Parses the NUL-separated porcelain v2 output.
pub fn parse(output: &str) -> Status {
    let mut status = Status::default();
    let mut records = output.split('\0').filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.head" if value != "(detached)" => status.branch = Some(value.to_owned()),
                "branch.upstream" => status.upstream = Some(value.to_owned()),
                "branch.ab" => {
                    for part in value.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            status.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            status.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let mut fields = record.splitn(2, ' ');
        let kind = fields.next().unwrap_or_default();
        let rest = fields.next().unwrap_or_default();
        // `<n fields> <path>`: the path is the rest after the fixed fields (it may hold spaces).
        let entry = |fixed: usize| -> Option<(char, char, String)> {
            let mut parts = rest.splitn(fixed + 1, ' ');
            let xy = parts.next()?;
            let path = parts.nth(fixed - 1)?;
            let mut letters = xy.chars();
            Some((letters.next()?, letters.next()?, path.to_owned()))
        };
        match kind {
            "1" => {
                if let Some((index, worktree, path)) = entry(7) {
                    status.files.push(FileStatus {
                        path,
                        original: None,
                        index,
                        worktree,
                        kind: Kind::Changed,
                    });
                }
            }
            "2" => {
                if let Some((index, worktree, path)) = entry(8) {
                    let original = records.next().map(str::to_owned);
                    status.files.push(FileStatus {
                        path,
                        original,
                        index,
                        worktree,
                        kind: Kind::Renamed,
                    });
                }
            }
            "u" => {
                if let Some((index, worktree, path)) = entry(9) {
                    status.files.push(FileStatus {
                        path,
                        original: None,
                        index,
                        worktree,
                        kind: Kind::Conflict,
                    });
                }
            }
            "?" => status.files.push(FileStatus {
                path: rest.to_owned(),
                original: None,
                index: '?',
                worktree: '?',
                kind: Kind::Untracked,
            }),
            _ => {}
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_and_every_kind_of_entry() {
        let output = [
            "# branch.oid 1234",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 aaa bbb src/App.java",
            "1 A. N... 000000 100644 100644 000 ccc src/New File.java",
            "2 R. N... 100644 100644 100644 ddd eee R100 src/Renamed.java",
            "src/Old.java",
            "u UU N... 100644 100644 100644 100644 f g h conflict.txt",
            "? notes.txt",
            "",
        ]
        .join("\0");
        let status = parse(&output);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        let summary: Vec<(&str, &str, bool)> = status
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.change(), f.staged()))
            .collect();
        assert_eq!(
            summary,
            [
                ("src/App.java", "modified", false),
                ("src/New File.java", "added", true),
                ("src/Renamed.java", "renamed", true),
                ("conflict.txt", "conflict", true),
                ("notes.txt", "untracked", false),
            ]
        );
        assert_eq!(status.files[2].original.as_deref(), Some("src/Old.java"));
    }

    #[test]
    fn detached_heads_have_no_branch() {
        assert_eq!(parse("# branch.head (detached)\0").branch, None);
    }
}
