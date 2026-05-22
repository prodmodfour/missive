//! Generic envelope primitives for durable missive records and event streams.

use serde::{Deserialize, Serialize};

use crate::{EventId, Metadata, MissiveTimestamp};

/// A typed payload with stable event identity, timestamp, and JSON metadata.
///
/// Later CLI, store, gateway, and adapter tickets can use this primitive to keep
/// machine-readable event records consistent without committing to a concrete
/// protocol payload shape in `missive-core`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<P> {
    /// Stable event identifier for this envelope.
    pub event_id: EventId,
    /// Timestamp at which the envelope was created or observed.
    pub timestamp: MissiveTimestamp,
    /// Extra structured metadata associated with the envelope.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
    /// Domain or protocol payload carried by the envelope.
    pub payload: P,
}

impl<P> Envelope<P> {
    /// Creates a payload envelope with empty metadata.
    #[must_use]
    pub fn new(event_id: EventId, timestamp: MissiveTimestamp, payload: P) -> Self {
        Self {
            event_id,
            timestamp,
            metadata: Metadata::new(),
            payload,
        }
    }

    /// Replaces envelope metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Maps the payload while preserving identity, timestamp, and metadata.
    pub fn map_payload<Q>(self, map: impl FnOnce(P) -> Q) -> Envelope<Q> {
        Envelope {
            event_id: self.event_id,
            timestamp: self.timestamp,
            metadata: self.metadata,
            payload: map(self.payload),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn envelope_serializes_with_metadata_and_payload() {
        let event_id = EventId::new("evt/local/0001").expect("valid event id");
        let timestamp = "2025-01-02T03:04:05Z"
            .parse::<MissiveTimestamp>()
            .expect("valid timestamp");
        let metadata =
            Metadata::try_from_iter([("source", json!("unit-test"))]).expect("valid metadata");
        let envelope =
            Envelope::new(event_id, timestamp, json!({"kind": "example"})).with_metadata(metadata);

        assert_eq!(
            serde_json::to_value(&envelope).expect("serialize envelope"),
            json!({
                "event_id": "evt/local/0001",
                "timestamp": "2025-01-02T03:04:05Z",
                "metadata": {"source": "unit-test"},
                "payload": {"kind": "example"}
            })
        );
    }

    #[test]
    fn envelope_deserializes_and_maps_payload() {
        let envelope = serde_json::from_value::<Envelope<String>>(json!({
            "event_id": "evt/local/0002",
            "timestamp": "2025-01-02T03:04:05Z",
            "payload": "ready"
        }))
        .expect("deserialize envelope");

        assert!(envelope.metadata.is_empty());

        let mapped = envelope.map_payload(|payload| payload.len());

        assert_eq!(mapped.payload, 5);
        assert_eq!(mapped.event_id.as_str(), "evt/local/0002");
    }
}
