//! Events service: typed notifications, filters, coalescing, delivery classes A/B
//! (spec, section 11; ADR-016).
//!
//! Every subscriber has its own bounded queue. On a full queue:
//! - a **class A** (state) event drops the subscription: the subscriber drains what was
//!   queued and then receives [`Delivery::Resync`] with its topics, after which it must
//!   re-read state and subscribe again;
//! - a **class B** (notification) event is dropped and counted ([`Subscriber::dropped`]).
//!
//! Events of one publisher reach every subscriber in publish order. Coalescing of bursty
//! sources (e.g. `fs.changed`) is done by the source before publishing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use globset::{Glob, GlobMatcher};
use serde_json::Value;
use tokio::sync::mpsc;

use kumiko_protocol::common::SubscriptionId;
use kumiko_service_api::{DeliveryClass, Event, EventSink};
use kumiko_types::Uri;

/// Default queue length of a subscriber.
pub const DEFAULT_CAPACITY: usize = 1024;

#[derive(Debug, thiserror::Error)]
#[error("invalid glob `{glob}`: {message}")]
pub struct FilterError {
    pub glob: String,
    pub message: String,
}

/// Which events a subscriber receives.
#[derive(Debug, Clone)]
pub struct EventFilter {
    topics: Vec<String>,
    glob: Option<GlobMatcher>,
}

impl EventFilter {
    /// Topics are exact names (`document.saved`), namespace prefixes (`document.*`) or `*`.
    pub fn topics<I, S>(topics: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            topics: topics.into_iter().map(Into::into).collect(),
            glob: None,
        }
    }

    /// Keeps only events about URIs matching the glob. For `file:` URIs the glob is matched
    /// against the path, otherwise against the whole URI. Batches such as `fs.changed` are
    /// narrowed to the matching entries.
    pub fn with_glob(mut self, glob: &str) -> Result<Self, FilterError> {
        let matcher = Glob::new(glob)
            .map_err(|e| FilterError {
                glob: glob.to_owned(),
                message: e.to_string(),
            })?
            .compile_matcher();
        self.glob = Some(matcher);
        Ok(self)
    }

    pub fn topic_list(&self) -> &[String] {
        &self.topics
    }

    fn matches_topic(&self, topic: &str) -> bool {
        self.topics.iter().any(|pattern| {
            pattern == "*"
                || pattern == topic
                || pattern.strip_suffix(".*").is_some_and(|ns| {
                    topic
                        .strip_prefix(ns)
                        .is_some_and(|rest| rest.starts_with('.'))
                })
        })
    }

    fn matches_uri(glob: &GlobMatcher, uri: &str) -> bool {
        match Uri::parse(uri).ok().and_then(|u| u.to_file_path()) {
            Some(path) => glob.is_match(path),
            None => glob.is_match(uri),
        }
    }

    /// The event as this subscriber should see it, or `None` if it does not match.
    fn apply(&self, event: &Event) -> Option<Event> {
        if !self.matches_topic(&event.topic) {
            return None;
        }
        let Some(glob) = &self.glob else {
            return Some(event.clone());
        };
        if let Some(uri) = event.payload.get("uri").and_then(Value::as_str) {
            return Self::matches_uri(glob, uri).then(|| event.clone());
        }
        if let Some(changes) = event.payload.get("changes").and_then(Value::as_array) {
            let kept: Vec<Value> = changes
                .iter()
                .filter(|change| {
                    change
                        .get("uri")
                        .and_then(Value::as_str)
                        .is_some_and(|uri| Self::matches_uri(glob, uri))
                })
                .cloned()
                .collect();
            if kept.is_empty() {
                return None;
            }
            let mut narrowed = event.clone();
            narrowed.payload["changes"] = Value::Array(kept);
            return Some(narrowed);
        }
        // Events without URIs are not narrowed by a glob.
        Some(event.clone())
    }
}

/// What a subscriber receives.
#[derive(Debug, Clone, PartialEq)]
pub enum Delivery {
    Event(Event),
    /// The subscription was dropped after an overflow of class A events: re-read the state
    /// of these topics and subscribe again.
    Resync(Vec<String>),
}

type ResyncSlot = Arc<Mutex<Option<Vec<String>>>>;

struct Subscription {
    id: SubscriptionId,
    filter: EventFilter,
    queue: mpsc::Sender<Event>,
    resync: ResyncSlot,
    dropped: Arc<AtomicU64>,
}

/// Receiving end of a subscription. Dropping it unsubscribes.
pub struct Subscriber {
    id: SubscriptionId,
    queue: mpsc::Receiver<Event>,
    resync: ResyncSlot,
    dropped: Arc<AtomicU64>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Subscriber {
    pub fn id(&self) -> SubscriptionId {
        self.id
    }

    /// Next delivery; `None` once the subscription ended (unsubscribed or bus dropped).
    pub async fn recv(&mut self) -> Option<Delivery> {
        match self.queue.recv().await {
            Some(event) => Some(Delivery::Event(event)),
            None => lock(&self.resync).take().map(Delivery::Resync),
        }
    }

    /// Next delivery without waiting.
    pub fn try_recv(&mut self) -> Option<Delivery> {
        match self.queue.try_recv() {
            Ok(event) => Some(Delivery::Event(event)),
            Err(mpsc::error::TryRecvError::Empty) => None,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                lock(&self.resync).take().map(Delivery::Resync)
            }
        }
    }

    /// Class B events lost because this subscriber's queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// The core event bus.
pub struct EventBus {
    subscriptions: Mutex<Vec<Subscription>>,
    next_id: AtomicU64,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subscriptions: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn subscribe(&self, filter: EventFilter) -> Subscriber {
        self.subscribe_with_capacity(filter, DEFAULT_CAPACITY)
    }

    pub fn subscribe_with_capacity(&self, filter: EventFilter, capacity: usize) -> Subscriber {
        let id = SubscriptionId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::channel(capacity.max(1));
        let resync = ResyncSlot::default();
        let dropped = Arc::new(AtomicU64::new(0));
        lock(&self.subscriptions).push(Subscription {
            id,
            filter,
            queue: tx,
            resync: resync.clone(),
            dropped: dropped.clone(),
        });
        Subscriber {
            id,
            queue: rx,
            resync,
            dropped,
        }
    }

    /// Ends a subscription; the subscriber drains its queue and then gets `None`.
    pub fn unsubscribe(&self, id: SubscriptionId) {
        lock(&self.subscriptions).retain(|s| s.id != id);
    }

    pub fn subscriber_count(&self) -> usize {
        lock(&self.subscriptions).len()
    }
}

impl EventSink for EventBus {
    fn publish(&self, event: Event) {
        let mut subscriptions = lock(&self.subscriptions);
        subscriptions.retain(|subscription| {
            let Some(delivered) = subscription.filter.apply(&event) else {
                return true;
            };
            match subscription.queue.try_send(delivered) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => match event.class {
                    DeliveryClass::Notification => {
                        subscription.dropped.fetch_add(1, Ordering::Relaxed);
                        true
                    }
                    DeliveryClass::State => {
                        tracing::warn!(
                            subscription = subscription.id.0,
                            topic = %event.topic,
                            "subscriber overflowed; dropping it and requesting resync"
                        );
                        *lock(&subscription.resync) = Some(subscription.filter.topics.clone());
                        false
                    }
                },
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use kumiko_protocol::events::{
        ConfigChanged, DocumentChanged, DocumentSaved, FsChange, FsChangeKind, FsChanged,
    };
    use kumiko_types::DocumentVersion;

    use super::*;

    fn saved(path: &str, version: u64) -> Event {
        Event::core(&DocumentSaved {
            uri: Uri::parse(format!("file://{path}")).unwrap(),
            version: DocumentVersion(version),
        })
    }

    fn changed(version: u64) -> Event {
        Event::core(&DocumentChanged {
            uri: Uri::parse("file:///w/a.txt").unwrap(),
            version: DocumentVersion(version),
            changes: vec![],
        })
    }

    fn fs_changed(paths: &[&str]) -> Event {
        Event::core(&FsChanged {
            changes: paths
                .iter()
                .map(|p| FsChange {
                    uri: Uri::parse(format!("file://{p}")).unwrap(),
                    kind: FsChangeKind::Changed,
                })
                .collect(),
        })
    }

    fn topics(sub: &mut Subscriber) -> Vec<String> {
        std::iter::from_fn(|| sub.try_recv())
            .map(|d| match d {
                Delivery::Event(e) => e.topic,
                Delivery::Resync(t) => format!("resync:{}", t.join(",")),
            })
            .collect()
    }

    #[test]
    fn topic_filters() {
        let bus = EventBus::new();
        let mut exact = bus.subscribe(EventFilter::topics(["document.saved"]));
        let mut prefix = bus.subscribe(EventFilter::topics(["document.*"]));
        let mut all = bus.subscribe(EventFilter::topics(["*"]));
        bus.publish(saved("/w/a.txt", 1));
        bus.publish(changed(2));
        bus.publish(Event::core(&ConfigChanged { keys: vec![] }));
        assert_eq!(topics(&mut exact), ["document.saved"]);
        assert_eq!(topics(&mut prefix), ["document.saved", "document.changed"]);
        assert_eq!(
            topics(&mut all),
            ["document.saved", "document.changed", "config.changed"]
        );
    }

    #[test]
    fn prefix_does_not_match_longer_namespaces() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe(EventFilter::topics(["doc.*"]));
        bus.publish(saved("/w/a.txt", 1));
        assert!(topics(&mut sub).is_empty());
    }

    #[test]
    fn glob_filters_and_narrows_batches() {
        let bus = EventBus::new();
        let filter = EventFilter::topics(["fs.changed", "document.saved"])
            .with_glob("**/*.toml")
            .unwrap();
        let mut toml = bus.subscribe(filter);
        bus.publish(fs_changed(&["/w/a.toml", "/w/out/x.o", "/w/b/c.toml"]));
        bus.publish(fs_changed(&["/w/only.o"]));
        bus.publish(saved("/w/settings.toml", 1));
        bus.publish(saved("/w/readme.md", 1));

        let Some(Delivery::Event(batch)) = toml.try_recv() else {
            panic!("expected the narrowed batch");
        };
        let batch = batch.decode::<FsChanged>().unwrap().unwrap();
        let paths: Vec<_> = batch.changes.iter().map(|c| c.uri.as_str()).collect();
        assert_eq!(paths, ["file:///w/a.toml", "file:///w/b/c.toml"]);
        assert_eq!(topics(&mut toml), ["document.saved"]);
    }

    #[test]
    fn class_a_overflow_drops_the_subscription_with_resync() {
        let bus = EventBus::new();
        let mut slow = bus.subscribe_with_capacity(EventFilter::topics(["document.*"]), 2);
        for version in 1..=3 {
            bus.publish(changed(version));
        }
        assert_eq!(bus.subscriber_count(), 0);
        bus.publish(changed(4));
        assert_eq!(
            topics(&mut slow),
            ["document.changed", "document.changed", "resync:document.*"]
        );
        assert!(slow.try_recv().is_none());
    }

    #[test]
    fn class_b_overflow_drops_and_counts() {
        let bus = EventBus::new();
        let mut slow = bus.subscribe_with_capacity(EventFilter::topics(["fs.changed"]), 1);
        for _ in 0..3 {
            bus.publish(fs_changed(&["/w/a"]));
        }
        assert_eq!(slow.dropped(), 2);
        assert_eq!(bus.subscriber_count(), 1);
        assert_eq!(topics(&mut slow), ["fs.changed"]);
    }

    #[test]
    fn dropped_subscribers_are_removed() {
        let bus = EventBus::new();
        let sub = bus.subscribe(EventFilter::topics(["*"]));
        drop(sub);
        bus.publish(changed(1));
        assert_eq!(bus.subscriber_count(), 0);

        let mut kept = bus.subscribe(EventFilter::topics(["*"]));
        bus.unsubscribe(kept.id());
        assert!(kept.try_recv().is_none());
    }

    #[tokio::test]
    async fn order_is_preserved() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe(EventFilter::topics(["document.changed"]));
        for version in 1..=100 {
            bus.publish(changed(version));
        }
        for version in 1..=100 {
            let Some(Delivery::Event(event)) = sub.recv().await else {
                panic!("missing event");
            };
            let payload = event.decode::<DocumentChanged>().unwrap().unwrap();
            assert_eq!(payload.version, DocumentVersion(version));
        }
    }
}
