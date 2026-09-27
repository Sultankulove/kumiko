//! Project service: workspace folders, project detection, exclusions (spec, section 11).
//!
//! The core does not know project types (P10). It keeps workspace folders and answers
//! `workspaceContains:<glob>` questions, from which plugins' activation events and their
//! own detection follow; project models come from build plugins later.
//!
//! Exclude globs apply to paths relative to a folder; a directory matching `<glob>` or
//! containing paths matching it (`**/out` and `**/out/**` alike) is not entered.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use globset::{Glob, GlobSet, GlobSetBuilder};

use kumiko_protocol::common::WorkspaceFolder;
use kumiko_protocol::events::{WorkspaceFolderAdded, WorkspaceFolderRemoved};
use kumiko_service_api::{Event, EventSink};
use kumiko_types::Uri;

/// Entries visited by one scan at most, so a huge tree cannot stall activation checks.
pub const DEFAULT_SCAN_LIMIT: usize = 200_000;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProjectError {
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("invalid glob `{glob}`: {message}")]
    Glob { glob: String, message: String },
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
}

fn glob_set(globs: &[String]) -> Result<GlobSet, ProjectError> {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(Glob::new(glob).map_err(|e| ProjectError::Glob {
            glob: glob.clone(),
            message: e.to_string(),
        })?);
    }
    builder.build().map_err(|e| ProjectError::Glob {
        glob: globs.join(", "),
        message: e.to_string(),
    })
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Folder {
    path: PathBuf,
    folder: WorkspaceFolder,
}

pub struct ProjectService {
    events: Arc<dyn EventSink>,
    folders: RwLock<Vec<Folder>>,
    exclude: RwLock<Arc<GlobSet>>,
    scan_limit: usize,
}

impl ProjectService {
    pub fn new(events: Arc<dyn EventSink>) -> Self {
        Self {
            events,
            folders: RwLock::default(),
            exclude: RwLock::new(Arc::new(GlobSet::empty())),
            scan_limit: DEFAULT_SCAN_LIMIT,
        }
    }

    pub fn with_scan_limit(mut self, limit: usize) -> Self {
        self.scan_limit = limit;
        self
    }

    /// Adds a folder (canonicalized); adding it again returns the existing entry.
    pub fn add_folder(&self, path: &Path) -> Result<WorkspaceFolder, ProjectError> {
        let path = kumiko_types::paths::canonicalize(path).map_err(|e| ProjectError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        if !path.is_dir() {
            return Err(ProjectError::NotADirectory(path));
        }
        let mut folders = write(&self.folders);
        if let Some(existing) = folders.iter().find(|f| f.path == path) {
            return Ok(existing.folder.clone());
        }
        let uri = Uri::from_file_path(&path).map_err(|e| ProjectError::Io {
            path: path.clone(),
            message: e.to_string(),
        })?;
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let folder = WorkspaceFolder { uri, name };
        folders.push(Folder {
            path,
            folder: folder.clone(),
        });
        drop(folders);
        self.events.publish(Event::core(&WorkspaceFolderAdded {
            folder: folder.clone(),
        }));
        Ok(folder)
    }

    pub fn remove_folder(&self, uri: &Uri) -> bool {
        let mut folders = write(&self.folders);
        let Some(index) = folders.iter().position(|f| &f.folder.uri == uri) else {
            return false;
        };
        let removed = folders.remove(index);
        drop(folders);
        self.events.publish(Event::core(&WorkspaceFolderRemoved {
            folder: removed.folder,
        }));
        true
    }

    pub fn folders(&self) -> Vec<WorkspaceFolder> {
        read(&self.folders)
            .iter()
            .map(|f| f.folder.clone())
            .collect()
    }

    pub fn folder_paths(&self) -> Vec<PathBuf> {
        read(&self.folders).iter().map(|f| f.path.clone()).collect()
    }

    /// The folder containing a path, if any.
    pub fn folder_of(&self, path: &Path) -> Option<WorkspaceFolder> {
        read(&self.folders)
            .iter()
            .filter(|f| path.starts_with(&f.path))
            .max_by_key(|f| f.path.as_os_str().len())
            .map(|f| f.folder.clone())
    }

    /// Whether `files.exclude` hides the path (relative to its folder); paths outside the
    /// workspace are not excluded.
    pub fn is_excluded(&self, path: &Path) -> bool {
        let folders = read(&self.folders);
        let Some(folder) = folders
            .iter()
            .filter(|f| path.starts_with(&f.path))
            .max_by_key(|f| f.path.as_os_str().len())
        else {
            return false;
        };
        let relative = path.strip_prefix(&folder.path).unwrap_or(path);
        let exclude = read(&self.exclude);
        // An excluded directory hides everything below it.
        relative
            .ancestors()
            .filter(|p| !p.as_os_str().is_empty())
            .any(|p| excluded(&exclude, p))
    }

    pub fn set_exclude(&self, globs: &[String]) -> Result<(), ProjectError> {
        *write(&self.exclude) = Arc::new(glob_set(globs)?);
        Ok(())
    }

    /// `workspaceContains:<glob>`: whether any folder has a file matching the glob
    /// (relative to the folder).
    pub async fn contains(&self, glob: &str) -> Result<bool, ProjectError> {
        Ok(!self.find_files(glob, 1).await?.is_empty())
    }

    /// Files matching a glob relative to their folder, at most `limit`.
    pub async fn find_files(&self, glob: &str, limit: usize) -> Result<Vec<Uri>, ProjectError> {
        let matcher = glob_set(&[glob.to_owned()])?;
        let roots = self.folder_paths();
        let exclude = read(&self.exclude).clone();
        let scan_limit = self.scan_limit;
        let found = tokio::task::spawn_blocking(move || {
            let mut found = Vec::new();
            let mut budget = scan_limit;
            for root in roots {
                scan(&root, &matcher, &exclude, limit, &mut budget, &mut found);
                if found.len() >= limit || budget == 0 {
                    break;
                }
            }
            found
        })
        .await
        .map_err(|e| ProjectError::Io {
            path: PathBuf::new(),
            message: e.to_string(),
        })?;
        Ok(found
            .into_iter()
            .filter_map(|path| Uri::from_file_path(path).ok())
            .collect())
    }
}

fn excluded(exclude: &GlobSet, relative: &Path) -> bool {
    exclude.is_match(relative) || exclude.is_match(relative.join("_"))
}

/// Breadth-first walk without following symlinked directories.
fn scan(
    root: &Path,
    matcher: &GlobSet,
    exclude: &GlobSet,
    limit: usize,
    budget: &mut usize,
    found: &mut Vec<PathBuf>,
) {
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if *budget == 0 || found.len() >= limit {
                return;
            }
            *budget -= 1;
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            if excluded(exclude, relative) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                queue.push_back(path);
            } else if matcher.is_match(relative) {
                found.push(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Collect(Mutex<Vec<String>>);

    impl EventSink for Collect {
        fn publish(&self, event: Event) {
            self.0.lock().unwrap().push(event.topic);
        }
    }

    fn touch(root: &Path, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn folders_are_added_once_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Collect::default());
        let project = ProjectService::new(events.clone());
        let folder = project.add_folder(dir.path()).unwrap();
        assert_eq!(project.add_folder(dir.path()).unwrap(), folder);
        assert_eq!(project.folders().len(), 1);
        assert!(folder.uri.as_str().starts_with("file:///"));

        let file = kumiko_types::paths::canonicalize(dir.path())
            .unwrap()
            .join("x.txt");
        assert_eq!(project.folder_of(&file), Some(folder.clone()));

        assert!(project.remove_folder(&folder.uri));
        assert!(!project.remove_folder(&folder.uri));
        assert_eq!(
            *events.0.lock().unwrap(),
            ["workspace.folderAdded", "workspace.folderRemoved"]
        );

        touch(dir.path(), "file.txt");
        assert!(matches!(
            project.add_folder(&dir.path().join("file.txt")),
            Err(ProjectError::NotADirectory(_))
        ));
    }

    #[tokio::test]
    async fn workspace_contains_respects_excludes() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "app/build.manifest");
        touch(dir.path(), "vendor/lib/build.manifest");
        touch(dir.path(), "src/main.txt");
        let project = ProjectService::new(Arc::new(Collect::default()));
        project.add_folder(dir.path()).unwrap();

        assert!(project.contains("**/build.manifest").await.unwrap());
        assert!(project.contains("src/*.txt").await.unwrap());
        assert!(!project.contains("**/*.bin").await.unwrap());
        assert_eq!(
            project
                .find_files("**/build.manifest", 10)
                .await
                .unwrap()
                .len(),
            2
        );

        project.set_exclude(&["**/vendor".into()]).unwrap();
        let root = kumiko_types::paths::canonicalize(dir.path()).unwrap();
        assert!(project.is_excluded(&root.join("vendor")));
        assert!(project.is_excluded(&root.join("vendor").join("lib")));
        assert!(!project.is_excluded(&root.join("app")));
        assert!(!project.is_excluded(&root));
        let found = project.find_files("**/build.manifest", 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].as_str().ends_with("/app/build.manifest"));
        project
            .set_exclude(&["**/app/**".into(), "**/vendor/**".into()])
            .unwrap();
        assert!(!project.contains("**/build.manifest").await.unwrap());
    }

    #[tokio::test]
    async fn scans_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            touch(dir.path(), &format!("d/f{i}.txt"));
        }
        let project = ProjectService::new(Arc::new(Collect::default())).with_scan_limit(10);
        project.add_folder(dir.path()).unwrap();
        assert!(project.find_files("**/*.txt", 100).await.unwrap().len() < 10);
        assert!(project.set_exclude(&["[".into()]).is_err());
    }
}
