//! Headless driver that runs scenarios through CoreApi without UI (spec, section 23).
//!
//! [`Headless`] starts a core with throwaway directories, the repository's built-in
//! resources and a temporary workspace, and offers helpers to wait for events. Integration
//! scenarios in `tests/` and the modularity tests drive the IDE through it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use kumiko_core::events::{Delivery, EventFilter, Subscriber};
use kumiko_core::service_api::Event;
use kumiko_core::{CoreApi, CoreError, CoreOptions};
use kumiko_types::KumikoDirs;

pub use kumiko_core;

/// Default time to wait for an event.
pub const WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum HeadlessError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("timed out after {0:?} waiting for {1}")]
    Timeout(Duration, String),
    #[error("the subscription ended while waiting for {0}")]
    Ended(String),
}

/// Built-in resources of this repository.
pub fn repository_resources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources")
}

/// A running core with temporary directories; everything is removed on drop.
pub struct Headless {
    pub core: CoreApi,
    home: tempfile::TempDir,
    workspace: PathBuf,
}

impl Headless {
    /// Starts a core with the repository resources and an empty workspace directory
    /// (not opened yet).
    pub async fn start() -> Result<Self, HeadlessError> {
        Self::start_with(|_| {}).await
    }

    /// Like [`Headless::start`], with a chance to adjust the options.
    pub async fn start_with(adjust: impl FnOnce(&mut CoreOptions)) -> Result<Self, HeadlessError> {
        let home = tempfile::tempdir()?;
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace)?;
        let mut options = CoreOptions::new(KumikoDirs::with_root(home.path().join("kumiko")));
        options.builtin_resources = Some(repository_resources());
        options.locale = Some("en".into());
        adjust(&mut options);
        let core = CoreApi::start(options).await?;
        Ok(Self {
            core,
            home,
            workspace,
        })
    }

    /// Root of the temporary directories.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// The temporary workspace directory.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Writes a file under the workspace, creating parent directories.
    pub fn write(&self, relative: &str, content: &str) -> Result<PathBuf, HeadlessError> {
        let path = self.workspace.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        Ok(path)
    }

    pub fn subscribe(&self, topics: &[&str]) -> Subscriber {
        self.core
            .events()
            .subscribe(EventFilter::topics(topics.iter().copied()))
    }
}

/// Waits for the first event accepted by `accept`, skipping others.
pub async fn wait_for(
    subscriber: &mut Subscriber,
    what: &str,
    mut accept: impl FnMut(&Event) -> bool,
) -> Result<Event, HeadlessError> {
    let search = async {
        loop {
            match subscriber.recv().await {
                Some(Delivery::Event(event)) if accept(&event) => return Ok(event),
                Some(_) => continue,
                None => return Err(HeadlessError::Ended(what.to_owned())),
            }
        }
    };
    tokio::time::timeout(WAIT, search)
        .await
        .map_err(|_| HeadlessError::Timeout(WAIT, what.to_owned()))?
}

/// Polls `condition` until it holds.
pub async fn eventually(
    what: &str,
    mut condition: impl FnMut() -> bool,
) -> Result<(), HeadlessError> {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !condition() {
        if tokio::time::Instant::now() > deadline {
            return Err(HeadlessError::Timeout(WAIT, what.to_owned()));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}
