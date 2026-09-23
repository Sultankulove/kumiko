use serde_json::Value;

use kumiko_protocol::events::{CoreEvent, DeliveryClass};

/// An event on the core bus: a topic, its delivery class and a JSON payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub topic: String,
    pub class: DeliveryClass,
    pub payload: Value,
}

impl Event {
    /// A core event with its typed payload.
    pub fn core<E: CoreEvent>(payload: &E) -> Self {
        Self {
            topic: E::TOPIC.to_owned(),
            class: E::CLASS,
            // Core payloads are plain data; serializing them cannot fail.
            payload: serde_json::to_value(payload).unwrap_or(Value::Null),
        }
    }

    /// The typed payload, if this is the core event `E`.
    pub fn decode<E: CoreEvent>(&self) -> Option<Result<E, serde_json::Error>> {
        (self.topic == E::TOPIC).then(|| E::deserialize(&self.payload))
    }
}

/// Where services publish events.
pub trait EventSink: Send + Sync {
    fn publish(&self, event: Event);
}

#[cfg(test)]
mod tests {
    use kumiko_protocol::events::{ConfigChanged, DocumentClosed};

    use super::*;

    #[test]
    fn core_events_round_trip() {
        let event = Event::core(&ConfigChanged {
            keys: vec!["editor.fontSize".into()],
        });
        assert_eq!(event.topic, "config.changed");
        assert_eq!(event.class, DeliveryClass::State);
        let decoded = event.decode::<ConfigChanged>().unwrap().unwrap();
        assert_eq!(decoded.keys, ["editor.fontSize"]);
        assert!(event.decode::<DocumentClosed>().is_none());
    }
}
