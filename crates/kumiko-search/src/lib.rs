//! Search service (spec, section 11): the FileIndex behind Quick Open and full-text search
//! over its files ([`text`]).
//!
//! The index lists the files of the workspace folders, respecting `.gitignore` and an
//! exclusion filter the core supplies (`files.exclude`), and answers fuzzy queries over the
//! path relative to the folder (nucleo, path-aware scoring).

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use kumiko_types::Uri;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub mod text;

/// A file in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    pub uri: Uri,
    /// Path relative to its workspace folder, `/`-separated: what queries match against.
    pub relative: String,
    /// Name of the workspace folder it belongs to.
    pub folder: String,
}

impl IndexedFile {
    /// The file name, the last segment of `relative`.
    pub fn name(&self) -> &str {
        self.relative.rsplit('/').next().unwrap_or(&self.relative)
    }
}

/// A query result: the file, its score and the matched character positions in `relative`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch {
    pub file: IndexedFile,
    pub score: u32,
    pub indices: Vec<u32>,
}

/// A workspace folder to index.
#[derive(Debug, Clone)]
pub struct IndexRoot {
    pub name: String,
    pub path: PathBuf,
}

/// Paths the index leaves out (with everything below them).
pub type Exclude = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

#[derive(Default)]
pub struct FileIndex {
    files: RwLock<Arc<Vec<IndexedFile>>>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.snapshot().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn snapshot(&self) -> Arc<Vec<IndexedFile>> {
        self.files
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Every file, in path order.
    pub fn files(&self) -> Arc<Vec<IndexedFile>> {
        self.snapshot()
    }

    /// Replaces the index with the files under `roots`. Blocking: walks the file system.
    /// Patterns in files named `ignore_files` apply (their names are data: settings, not code),
    /// hidden files are listed, and `exclude` hides paths with everything below them.
    pub fn rebuild(&self, roots: &[IndexRoot], ignore_files: &[String], exclude: Exclude) {
        let mut files = Vec::new();
        for root in roots {
            let exclude = exclude.clone();
            let mut walk = ignore::WalkBuilder::new(&root.path);
            walk.standard_filters(false).follow_links(false);
            for name in ignore_files {
                walk.add_custom_ignore_filename(name);
            }
            let walk = walk
                .filter_entry(move |entry| !exclude(entry.path()))
                .build();
            for entry in walk.flatten() {
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let path = entry.path();
                let Ok(relative) = path.strip_prefix(&root.path) else {
                    continue;
                };
                let Ok(uri) = Uri::from_file_path(path) else {
                    continue;
                };
                let relative: Vec<String> = relative
                    .iter()
                    .map(|c| c.to_string_lossy().into_owned())
                    .collect();
                files.push(IndexedFile {
                    uri,
                    relative: relative.join("/"),
                    folder: root.name.clone(),
                });
            }
        }
        files.sort_by(|a, b| a.relative.cmp(&b.relative).then_with(|| a.uri.cmp(&b.uri)));
        *self
            .files
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(files);
    }

    /// The best `limit` files for a fuzzy query; an empty query lists files in path order.
    pub fn find(&self, query: &str, limit: usize) -> Vec<FileMatch> {
        let files = self.snapshot();
        if query.trim().is_empty() {
            return files
                .iter()
                .take(limit)
                .map(|file| FileMatch {
                    file: file.clone(),
                    score: 0,
                    indices: Vec::new(),
                })
                .collect();
        }
        rank(
            query,
            files.iter().map(|f| f.relative.as_str()),
            limit,
            true,
        )
        .into_iter()
        .map(|(i, score, indices)| FileMatch {
            file: files[i].clone(),
            score,
            indices,
        })
        .collect()
    }
}

/// Fuzzy-ranks `candidates` against `query`: `(index, score, matched char positions)`, best
/// first, ties broken by shorter text then original order. `paths` enables path-aware scoring.
/// Lists from this length on are scored on several threads.
const PARALLEL_FROM: usize = 4096;

/// Scores `texts`, numbering them from `first`.
fn score_chunk<'a>(
    texts: &[&'a str],
    first: usize,
    pattern: &Pattern,
    matcher: &mut Matcher,
    buffer: &mut Vec<char>,
) -> Vec<(usize, u32, &'a str)> {
    texts
        .iter()
        .enumerate()
        .filter_map(|(i, text)| {
            let score = pattern.score(Utf32Str::new(text, buffer), matcher)?;
            Some((first + i, score, *text))
        })
        .collect()
}

pub fn rank<'a>(
    query: &str,
    candidates: impl Iterator<Item = &'a str>,
    limit: usize,
    paths: bool,
) -> Vec<(usize, u32, Vec<u32>)> {
    let config = if paths {
        Config::DEFAULT.match_paths()
    } else {
        Config::DEFAULT
    };
    let mut matcher = Matcher::new(config.clone());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buffer = Vec::new();
    let texts: Vec<&str> = candidates.collect();
    // Big lists are scored on several threads, each with a matcher of its own: Quick Open on
    // 50 000 files has 30 ms per typed character.
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let mut scored: Vec<(usize, u32, &str)> = if texts.len() < PARALLEL_FROM || threads < 2 {
        score_chunk(&texts, 0, &pattern, &mut matcher, &mut buffer)
    } else {
        let size = texts.len().div_ceil(threads);
        std::thread::scope(|scope| {
            let handles: Vec<_> = texts
                .chunks(size)
                .enumerate()
                .map(|(n, chunk)| {
                    let (pattern, config) = (&pattern, config.clone());
                    scope.spawn(move || {
                        let mut matcher = Matcher::new(config);
                        let mut buffer = Vec::new();
                        score_chunk(chunk, n * size, pattern, &mut matcher, &mut buffer)
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        })
    };
    scored.sort_by_key(|&(i, score, text)| (Reverse(score), text.len(), i));
    scored.truncate(limit);
    // Match positions only for what is shown: they cost a second pass.
    scored
        .into_iter()
        .map(|(i, score, text)| {
            let mut indices = Vec::new();
            pattern.indices(Utf32Str::new(text, &mut buffer), &mut matcher, &mut indices);
            indices.sort_unstable();
            indices.dedup();
            (i, score, indices)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        dir
    }

    fn index(dir: &Path, exclude: Exclude) -> FileIndex {
        let index = FileIndex::new();
        let root = IndexRoot {
            name: "w".into(),
            path: dir.to_path_buf(),
        };
        index.rebuild(&[root], &[".gitignore".to_owned()], exclude);
        index
    }

    fn relative(matches: &[FileMatch]) -> Vec<&str> {
        matches.iter().map(|m| m.file.relative.as_str()).collect()
    }

    #[test]
    fn index_respects_gitignore_and_excludes() {
        let dir = tree(&[
            "src/main.rs",
            "src/lib.rs",
            ".github/workflows/ci.yml",
            ".git/config",
            "target/debug/app",
            "vendor/dep.rs",
            ".gitignore",
        ]);
        std::fs::write(dir.path().join(".gitignore"), "/target\n").unwrap();
        // The core passes `files.exclude` here, which hides VCS directories by default.
        let (vendor, vcs) = (dir.path().join("vendor"), dir.path().join(".git"));
        let index = index(
            dir.path(),
            Arc::new(move |p: &Path| p == vendor || p == vcs),
        );
        assert_eq!(
            relative(&index.find("", 100)),
            [
                ".github/workflows/ci.yml",
                ".gitignore",
                "src/lib.rs",
                "src/main.rs"
            ]
        );
        assert_eq!(index.find("", 100)[3].file.name(), "main.rs");
    }

    #[test]
    fn fuzzy_queries_rank_file_names_first() {
        let dir = tree(&[
            "crates/kumiko-ui/src/workbench.rs",
            "crates/kumiko-ui/src/tree.rs",
            "docs/workflow.md",
            "README.md",
        ]);
        let index = index(dir.path(), Arc::new(|_: &Path| false));
        let found = index.find("wb", 10);
        assert_eq!(found[0].file.relative, "crates/kumiko-ui/src/workbench.rs");
        assert_eq!(relative(&index.find("readme", 10)), ["README.md"]);
        assert!(index.find("zzz", 10).is_empty());
        assert_eq!(index.find("r", 2).len(), 2);
    }

    #[test]
    fn rank_prefers_better_then_shorter_matches() {
        let items = [
            "Toggle Project",
            "Open File",
            "Close Tab",
            "Project Structure",
        ];
        let ranked = rank("proj", items.iter().copied(), 10, false);
        let names: Vec<&str> = ranked.iter().map(|(i, _, _)| items[*i]).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"Toggle Project") && names.contains(&"Project Structure"));
    }
}

#[cfg(test)]
mod parallel_tests {
    use super::*;

    #[test]
    fn big_lists_rank_like_small_ones() {
        let names: Vec<String> = (0..PARALLEL_FROM * 3)
            .map(|i| format!("module{:04}/src/Class{:05}Service.java", i / 100, i))
            .collect();
        let big = rank("class01234svc", names.iter().map(String::as_str), 5, true);
        // The same candidates in a list too short for threads.
        let small: Vec<&str> = names[1000..1300].iter().map(String::as_str).collect();
        let small = rank("class01234svc", small.into_iter(), 5, true);
        assert_eq!(names[big[0].0], "module0012/src/Class01234Service.java");
        assert_eq!(
            big[0].0,
            small[0].0 + 1000,
            "the same winner, with its own index"
        );
    }
}
