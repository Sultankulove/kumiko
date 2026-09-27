//! Output channels (spec, section 20: the Output panel): named, append-only text logs the UI
//! shows read-only. The IDE log is one; plugins get theirs with the `ui/*` host methods.
//!
//! Readers watch a version counter instead of events, so appending (possibly many lines per
//! second from logging) never floods the event bus.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, RwLock};

use tokio::sync::watch;

/// Lines a channel keeps; older ones are dropped.
pub const CHANNEL_CAPACITY: usize = 10_000;

/// The IDE's own log.
pub const LOG_CHANNEL: &str = "kumiko.log";

#[derive(Default)]
struct Channel {
    lines: VecDeque<String>,
    /// Lines dropped from the front so far.
    dropped: u64,
}

/// All output channels; cheap to clone.
#[derive(Clone)]
pub struct Output {
    channels: Arc<RwLock<BTreeMap<String, Channel>>>,
    version: Arc<watch::Sender<u64>>,
}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("channels", &self.channels())
            .finish()
    }
}

impl Default for Output {
    fn default() -> Self {
        Self::new()
    }
}

impl Output {
    pub fn new() -> Self {
        Self {
            channels: Arc::default(),
            version: Arc::new(watch::channel(0).0),
        }
    }

    /// Appends text to a channel, creating it; the text may hold several lines.
    pub fn append(&self, channel: &str, text: &str) {
        {
            let mut channels = self
                .channels
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let channel = channels.entry(channel.to_owned()).or_default();
            for line in text.lines() {
                channel.lines.push_back(line.to_owned());
            }
            while channel.lines.len() > CHANNEL_CAPACITY {
                channel.lines.pop_front();
                channel.dropped += 1;
            }
        }
        self.version.send_modify(|v| *v += 1);
    }

    pub fn clear(&self, channel: &str) {
        if let Some(channel) = self
            .channels
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(channel)
        {
            channel.dropped += channel.lines.len() as u64;
            channel.lines.clear();
        }
        self.version.send_modify(|v| *v += 1);
    }

    /// Channel ids in order.
    pub fn channels(&self) -> Vec<String> {
        self.channels
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// The lines a channel holds now.
    pub fn lines(&self, channel: &str) -> Vec<String> {
        self.channels
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(channel)
            .map(|c| c.lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Changes whenever any channel changes.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_append_split_and_cap() {
        let output = Output::new();
        let mut changes = output.watch();
        output.append("b", "one\ntwo");
        output.append("a", "x");
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        assert_eq!(output.channels(), ["a", "b"]);
        assert_eq!(output.lines("b"), ["one", "two"]);
        for i in 0..CHANNEL_CAPACITY + 5 {
            output.append("b", &i.to_string());
        }
        let lines = output.lines("b");
        assert_eq!(lines.len(), CHANNEL_CAPACITY);
        assert_eq!(lines.last().unwrap(), &(CHANNEL_CAPACITY + 4).to_string());
        output.clear("b");
        assert!(output.lines("b").is_empty());
        assert!(changes.has_changed().unwrap());
    }
}
