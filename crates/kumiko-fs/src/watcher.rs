//! Recursive watcher of workspace roots with coalescing (spec, sections 11 and 24).
//!
//! Raw notifications are collected for `window` after the first one, merged per path and
//! published as one `fs.changed` batch, so a storm (a build writing thousands of files)
//! becomes a few events. Paths matching the exclude globs are dropped. If the kernel limit
//! of watches is exhausted, the root falls back to polling and a warning names the fix.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::time::Instant;

use kumiko_protocol::events::{FsChange, FsChangeKind, FsChanged};
use kumiko_service_api::{Event, EventSink};
use kumiko_types::Uri;

#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Coalescing window, 50-100 ms by the spec.
    pub window: Duration,
    /// Globs over absolute paths, e.g. `**/node_modules/**`.
    pub exclude: Vec<String>,
    /// Interval of the polling fallback.
    pub poll_interval: Duration,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_millis(75),
            exclude: Vec::new(),
            poll_interval: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("invalid exclude glob `{glob}`: {message}")]
    Glob { glob: String, message: String },
    #[error("cannot watch {path}: {message}")]
    Watch { path: PathBuf, message: String },
}

fn build_excludes(globs: &[String]) -> Result<GlobSet, WatchError> {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(Glob::new(glob).map_err(|e| WatchError::Glob {
            glob: glob.clone(),
            message: e.to_string(),
        })?);
    }
    builder.build().map_err(|e| WatchError::Glob {
        glob: globs.join(", "),
        message: e.to_string(),
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

type Raw = (PathBuf, FsChangeKind);

struct Backends {
    native: RecommendedWatcher,
    poll: Option<PollWatcher>,
    polled_roots: Vec<PathBuf>,
}

/// Watches roots and publishes `fs.changed`. Stops when dropped.
pub struct FsWatcher {
    backends: Mutex<Backends>,
    excludes: Arc<Mutex<GlobSet>>,
    raw: mpsc::UnboundedSender<Raw>,
    poll_interval: Duration,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FsWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn notify_handler(
    raw: mpsc::UnboundedSender<Raw>,
) -> impl Fn(notify::Result<notify::Event>) + Send {
    move |result: notify::Result<notify::Event>| {
        let event = match result {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(%error, "file watcher error");
                return;
            }
        };
        let kinds: Vec<FsChangeKind> = match event.kind {
            EventKind::Access(_) => return,
            EventKind::Create(_) => vec![FsChangeKind::Created],
            EventKind::Remove(_) => vec![FsChangeKind::Deleted],
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => vec![FsChangeKind::Deleted],
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => vec![FsChangeKind::Created],
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                vec![FsChangeKind::Deleted, FsChangeKind::Created]
            }
            _ => vec![FsChangeKind::Changed],
        };
        for (i, path) in event.paths.into_iter().enumerate() {
            let kind = kinds.get(i).or(kinds.last()).copied();
            if let Some(kind) = kind {
                let _ = raw.send((path, kind));
            }
        }
    }
}

impl FsWatcher {
    /// Starts the coalescing task on the current Tokio runtime.
    pub fn start(events: Arc<dyn EventSink>, config: WatcherConfig) -> Result<Self, WatchError> {
        let excludes = Arc::new(Mutex::new(build_excludes(&config.exclude)?));
        let (raw_tx, raw_rx) = mpsc::unbounded_channel();
        let native =
            RecommendedWatcher::new(notify_handler(raw_tx.clone()), notify::Config::default())
                .map_err(|e| WatchError::Watch {
                    path: PathBuf::new(),
                    message: e.to_string(),
                })?;
        let task = tokio::spawn(coalesce(raw_rx, events, excludes.clone(), config.window));
        Ok(Self {
            backends: Mutex::new(Backends {
                native,
                poll: None,
                polled_roots: Vec::new(),
            }),
            excludes,
            raw: raw_tx,
            poll_interval: config.poll_interval,
            task,
        })
    }

    /// Watches a directory tree.
    pub fn watch(&self, root: &Path) -> Result<(), WatchError> {
        let mut backends = lock(&self.backends);
        let error = match backends.native.watch(root, RecursiveMode::Recursive) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if !matches!(error.kind, notify::ErrorKind::MaxFilesWatch) {
            return Err(WatchError::Watch {
                path: root.to_path_buf(),
                message: error.to_string(),
            });
        }
        tracing::warn!(
            root = %root.display(),
            "the inotify watch limit is exhausted; falling back to polling. Raise it with \
             `sudo sysctl fs.inotify.max_user_watches=524288` (persist in /etc/sysctl.d/)"
        );
        let _ = backends.native.unwatch(root);
        if backends.poll.is_none() {
            let config = notify::Config::default().with_poll_interval(self.poll_interval);
            let poll = PollWatcher::new(notify_handler(self.raw.clone()), config).map_err(|e| {
                WatchError::Watch {
                    path: root.to_path_buf(),
                    message: e.to_string(),
                }
            })?;
            backends.poll = Some(poll);
        }
        if let Some(poll) = backends.poll.as_mut() {
            poll.watch(root, RecursiveMode::Recursive)
                .map_err(|e| WatchError::Watch {
                    path: root.to_path_buf(),
                    message: e.to_string(),
                })?;
        }
        backends.polled_roots.push(root.to_path_buf());
        Ok(())
    }

    pub fn unwatch(&self, root: &Path) {
        let mut backends = lock(&self.backends);
        if let Some(i) = backends.polled_roots.iter().position(|r| r == root) {
            backends.polled_roots.remove(i);
            if let Some(poll) = backends.poll.as_mut() {
                let _ = poll.unwatch(root);
            }
        } else {
            let _ = backends.native.unwatch(root);
        }
    }

    /// Replaces the exclude globs.
    pub fn set_exclude(&self, globs: &[String]) -> Result<(), WatchError> {
        *lock(&self.excludes) = build_excludes(globs)?;
        Ok(())
    }
}

/// Merges a new raw change into the pending kind of the same path; `None` removes it.
fn merge(previous: FsChangeKind, next: FsChangeKind) -> Option<FsChangeKind> {
    use FsChangeKind::*;
    match (previous, next) {
        (Created, Deleted) => None,
        (Created, _) => Some(Created),
        (Deleted, Created) => Some(Changed),
        (Deleted, Changed) => Some(Changed),
        (_, next) => Some(next),
    }
}

async fn coalesce(
    mut raw: mpsc::UnboundedReceiver<Raw>,
    events: Arc<dyn EventSink>,
    excludes: Arc<Mutex<GlobSet>>,
    window: Duration,
) {
    let mut order: Vec<PathBuf> = Vec::new();
    let mut pending: HashMap<PathBuf, FsChangeKind> = HashMap::new();
    let mut deadline: Option<Instant> = None;
    loop {
        let flush_at = deadline;
        tokio::select! {
            received = raw.recv() => {
                let Some((path, kind)) = received else { break };
                if lock(&excludes).is_match(&path) {
                    continue;
                }
                match pending.get(&path).copied() {
                    None => {
                        order.push(path.clone());
                        pending.insert(path, kind);
                    }
                    Some(previous) => match merge(previous, kind) {
                        Some(kind) => { pending.insert(path, kind); }
                        None => { pending.remove(&path); }
                    },
                }
                deadline.get_or_insert_with(|| Instant::now() + window);
            }
            () = async { tokio::time::sleep_until(flush_at.unwrap_or_else(Instant::now)).await },
                if flush_at.is_some() =>
            {
                deadline = None;
                let changes: Vec<FsChange> = order
                    .drain(..)
                    .filter_map(|path| {
                        let kind = pending.remove(&path)?;
                        let uri = Uri::from_file_path(&path).ok()?;
                        Some(FsChange { uri, kind })
                    })
                    .collect();
                pending.clear();
                if !changes.is_empty() {
                    events.publish(Event::core(&FsChanged { changes }));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tokio::sync::Notify;

    use super::*;

    #[derive(Default)]
    struct Collect {
        events: Mutex<Vec<FsChanged>>,
        notify: Notify,
    }

    impl EventSink for Collect {
        fn publish(&self, event: Event) {
            if let Some(Ok(batch)) = event.decode::<FsChanged>() {
                lock(&self.events).push(batch);
                self.notify.notify_waiters();
            }
        }
    }

    impl Collect {
        /// Waits until the merged view contains `expected` (or times out), returns batches.
        async fn wait_for(&self, done: impl Fn(&BTreeMap<String, FsChangeKind>) -> bool) -> usize {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let notified = self.notify.notified();
                {
                    let batches = lock(&self.events);
                    let merged: BTreeMap<String, FsChangeKind> = batches
                        .iter()
                        .flat_map(|b| b.changes.iter())
                        .map(|c| (c.uri.to_file_path().unwrap().display().to_string(), c.kind))
                        .collect();
                    if done(&merged) {
                        return batches.len();
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out; got {:?}",
                    lock(&self.events)
                );
                let _ = tokio::time::timeout(Duration::from_millis(200), notified).await;
            }
        }
    }

    fn start(sink: &Arc<Collect>, exclude: &[&str]) -> FsWatcher {
        let config = WatcherConfig {
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            ..WatcherConfig::default()
        };
        FsWatcher::start(sink.clone(), config).unwrap()
    }

    #[test]
    fn merging_rules() {
        use FsChangeKind::*;
        assert_eq!(merge(Created, Changed), Some(Created));
        assert_eq!(merge(Created, Deleted), None);
        assert_eq!(merge(Deleted, Created), Some(Changed));
        assert_eq!(merge(Changed, Deleted), Some(Deleted));
        assert_eq!(merge(Changed, Changed), Some(Changed));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reports_created_changed_and_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("existing.txt"), "a").unwrap();
        std::fs::write(root.join("doomed.txt"), "a").unwrap();
        let sink = Arc::new(Collect::default());
        let watcher = start(&sink, &[]);
        watcher.watch(&root).unwrap();

        std::fs::write(root.join("new.txt"), "x").unwrap();
        std::fs::write(root.join("existing.txt"), "b").unwrap();
        std::fs::remove_file(root.join("doomed.txt")).unwrap();

        let path = |name: &str| root.join(name).display().to_string();
        sink.wait_for(|m| {
            m.get(&path("new.txt")) == Some(&FsChangeKind::Created)
                && m.get(&path("existing.txt")) == Some(&FsChangeKind::Changed)
                && m.get(&path("doomed.txt")) == Some(&FsChangeKind::Deleted)
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn storm_is_coalesced_and_excludes_apply() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let sink = Arc::new(Collect::default());
        let watcher = start(&sink, &["**/out/**"]);
        watcher.watch(&root).unwrap();

        for i in 0..2000 {
            std::fs::write(root.join(format!("out/f{i}.o")), "x").unwrap();
        }
        for i in 0..200 {
            std::fs::write(root.join(format!("src/s{i}.txt")), "x").unwrap();
        }
        let batches = sink
            .wait_for(|m| {
                (0..200).all(|i| {
                    m.contains_key(&root.join(format!("src/s{i}.txt")).display().to_string())
                })
            })
            .await;
        let events = lock(&sink.events);
        let excluded = events
            .iter()
            .flat_map(|b| b.changes.iter())
            .filter(|c| c.uri.as_str().contains("/out/"))
            .count();
        assert_eq!(excluded, 0, "excluded paths leaked");
        assert!(batches <= 20, "2200 writes produced {batches} batches");
        drop(watcher);
    }
}
