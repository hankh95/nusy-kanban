//! Shared types, configuration, and error handling for noesis-ship.
//!
//! This module defines the core data types used across all noesis-ship primitives:
//!
//! - **Event types**: [`ShipEvent`], [`Event`], [`ChannelMessage`]
//! - **Config types**: [`NatsConfig`], [`StreamConfig`], [`KvBucketConfig`]
//! - **Error types**: [`Error`] enum with [`Result`] alias

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ─── Error Types ────────────────────────────────────────────────────────────

/// Unified error type for all noesis-ship operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// NATS connection failure.
    #[error("connection error: {0}")]
    Connection(String),

    /// JetStream operation failure.
    #[error("jetstream error: {0}")]
    JetStream(String),

    /// Serialization or deserialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Operation timed out.
    #[error("operation timed out after {0:?}")]
    Timeout(Duration),

    /// Attempted operation on a disconnected client.
    #[error("not connected to NATS server")]
    NotConnected,

    /// Key not found in KV store.
    #[error("key not found: {0}")]
    KeyNotFound(String),

    /// Object not found in object store.
    #[error("object not found: {0}")]
    ObjectNotFound(String),

    /// Bucket not found.
    #[error("bucket not found: {0}")]
    BucketNotFound(String),

    /// Stream already exists (non-fatal, usually mapped to Ok).
    #[error("stream already exists: {0}")]
    StreamExists(String),
}

/// Result alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

// ─── Event Types ────────────────────────────────────────────────────────────

/// Durable event envelope for the EventBus (JetStream).
///
/// Wraps a payload with metadata: timestamp, source, correlation ID, and version.
/// Ported from Python `event_bus.py:34-48`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShipEvent {
    /// Event type identifier (e.g., "kanban.item.created").
    pub event_type: String,

    /// RFC 3339 UTC timestamp of when the event was created.
    pub timestamp: String,

    /// Source identifier (service or agent that emitted the event).
    pub source: String,

    /// Event payload as a JSON value.
    pub payload: serde_json::Value,

    /// 8-character correlation ID (UUID v4 prefix) for tracing related events.
    pub correlation_id: String,

    /// Event envelope version.
    pub version: String,
}

impl ShipEvent {
    /// Create a new ShipEvent with auto-generated timestamp and correlation ID.
    pub fn new(
        event_type: impl Into<String>,
        source: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_type: event_type.into(),
            timestamp: Utc::now().to_rfc3339(),
            source: source.into(),
            payload,
            correlation_id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            version: "1.0".to_string(),
        }
    }

    /// Create a new ShipEvent with an explicit correlation ID.
    pub fn with_correlation(
        event_type: impl Into<String>,
        source: impl Into<String>,
        payload: serde_json::Value,
        correlation_id: impl Into<String>,
    ) -> Self {
        Self {
            event_type: event_type.into(),
            timestamp: Utc::now().to_rfc3339(),
            source: source.into(),
            payload,
            correlation_id: correlation_id.into(),
            version: "1.0".to_string(),
        }
    }
}

/// Lightweight event for PubSub (NATS Core, no persistence).
///
/// Ported from Python `pubsub.py:80-95`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Event type identifier.
    pub event_type: String,

    /// Source identifier.
    pub source: String,

    /// Event payload as a JSON value.
    pub payload: serde_json::Value,

    /// RFC 3339 UTC timestamp.
    pub timestamp: String,

    /// Optional correlation ID for tracing.
    pub correlation_id: Option<String>,
}

impl Event {
    /// Create a new Event with auto-generated timestamp.
    pub fn new(
        event_type: impl Into<String>,
        source: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_type: event_type.into(),
            source: source.into(),
            payload,
            timestamp: Utc::now().to_rfc3339(),
            correlation_id: None,
        }
    }
}

/// Message in a durable channel (JetStream point-to-point).
///
/// Ported from Python `channels.py:25-46`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// Sender identifier (agent or being).
    pub sender: String,

    /// Message content.
    pub content: String,

    /// Unix timestamp as f64 (nanosecond precision available).
    pub timestamp: f64,

    /// Channel name.
    pub channel: String,

    /// Unique message ID (format: `{channel}-{timestamp_ns}`).
    pub message_id: String,

    /// Optional metadata as a JSON value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ChannelMessage {
    /// Create a new ChannelMessage with auto-generated timestamp and ID.
    pub fn new(
        sender: impl Into<String>,
        content: impl Into<String>,
        channel: impl Into<String>,
    ) -> Self {
        let channel = channel.into();
        let now = Utc::now();
        let ts = now.timestamp() as f64 + now.timestamp_subsec_nanos() as f64 / 1_000_000_000.0;
        let message_id = format!("{}-{}", channel, now.timestamp_nanos_opt().unwrap_or(0));

        Self {
            sender: sender.into(),
            content: content.into(),
            timestamp: ts,
            channel,
            message_id,
            metadata: None,
        }
    }

    /// Create a ChannelMessage with metadata.
    pub fn with_metadata(
        sender: impl Into<String>,
        content: impl Into<String>,
        channel: impl Into<String>,
        metadata: serde_json::Value,
    ) -> Self {
        let mut msg = Self::new(sender, content, channel);
        msg.metadata = Some(metadata);
        msg
    }
}

// ─── Config Types ───────────────────────────────────────────────────────────

/// NATS connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatsConfig {
    /// NATS server URL (e.g., "nats://localhost:4222").
    pub url: String,

    /// Connection timeout.
    #[serde(with = "duration_serde")]
    pub connect_timeout: Duration,

    /// Request timeout for operations.
    #[serde(with = "duration_serde")]
    pub request_timeout: Duration,
}

impl NatsConfig {
    /// Create a new NatsConfig with default timeouts.
    ///
    /// Defaults: connect_timeout = 5s, request_timeout = 10s.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
        }
    }

    /// Override the connect timeout.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Override the request timeout.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }
}

impl Default for NatsConfig {
    fn default() -> Self {
        Self::new("nats://localhost:4222")
    }
}

/// JetStream stream configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamConfig {
    /// Stream name (e.g., "SHIP_EVENTS").
    pub name: String,

    /// Subject patterns this stream captures (e.g., `["ship.events.>"]`).
    pub subjects: Vec<String>,

    /// Maximum age of messages in seconds.
    pub max_age_secs: u64,

    /// Maximum number of messages.
    pub max_msgs: i64,

    /// Storage type: "file" or "memory".
    pub storage: String,
}

impl StreamConfig {
    /// Create a new StreamConfig.
    pub fn new(name: impl Into<String>, subjects: Vec<String>) -> Self {
        Self {
            name: name.into(),
            subjects,
            max_age_secs: 86400,
            max_msgs: 100_000,
            storage: "file".to_string(),
        }
    }

    /// Override max age.
    pub fn with_max_age(mut self, secs: u64) -> Self {
        self.max_age_secs = secs;
        self
    }

    /// Override max messages.
    pub fn with_max_msgs(mut self, max: i64) -> Self {
        self.max_msgs = max;
        self
    }

    /// Use memory storage instead of file.
    pub fn with_memory_storage(mut self) -> Self {
        self.storage = "memory".to_string();
        self
    }
}

/// NATS KV bucket configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvBucketConfig {
    /// Bucket name (e.g., "beings_status").
    pub bucket: String,

    /// Number of historical values to keep per key.
    pub history: u64,

    /// Time-to-live for entries (None = no expiration).
    #[serde(with = "option_duration_serde")]
    pub ttl: Option<Duration>,
}

impl KvBucketConfig {
    /// Create a new KvBucketConfig with no TTL.
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            history: 5,
            ttl: None,
        }
    }

    /// Set TTL in seconds.
    pub fn with_ttl_secs(mut self, secs: u64) -> Self {
        self.ttl = Some(Duration::from_secs(secs));
        self
    }

    /// Set history depth.
    pub fn with_history(mut self, history: u64) -> Self {
        self.history = history;
        self
    }
}

// ─── Serde helpers for Duration ─────────────────────────────────────────────

mod duration_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
}

mod option_duration_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.serialize_some(&d.as_secs()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        let secs: Option<u64> = Option::deserialize(d)?;
        Ok(secs.map(Duration::from_secs))
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ShipEvent tests

    #[test]
    fn ship_event_new_generates_timestamp() {
        let event = ShipEvent::new("test.event", "test-source", json!({"key": "value"}));
        assert!(!event.timestamp.is_empty());
        assert!(event.timestamp.contains('T')); // RFC3339
    }

    #[test]
    fn ship_event_new_generates_correlation_id() {
        let event = ShipEvent::new("test.event", "test-source", json!(null));
        assert_eq!(event.correlation_id.len(), 8);
    }

    #[test]
    fn ship_event_defaults_version_to_1_0() {
        let event = ShipEvent::new("test.event", "test-source", json!(null));
        assert_eq!(event.version, "1.0");
    }

    #[test]
    fn ship_event_serialization_roundtrip() {
        let event = ShipEvent::new("test.event", "source", json!({"count": 42}));
        let json_str = serde_json::to_string(&event).unwrap();
        let deserialized: ShipEvent = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.event_type, "test.event");
        assert_eq!(deserialized.source, "source");
        assert_eq!(deserialized.payload, json!({"count": 42}));
        assert_eq!(deserialized.version, "1.0");
    }

    #[test]
    fn ship_event_with_correlation() {
        let event = ShipEvent::with_correlation("test", "src", json!(null), "abcd1234");
        assert_eq!(event.correlation_id, "abcd1234");
    }

    // Event tests

    #[test]
    fn event_new_generates_timestamp() {
        let event = Event::new("heartbeat", "agent-1", json!(null));
        assert!(!event.timestamp.is_empty());
        assert!(event.timestamp.contains('T'));
    }

    #[test]
    fn event_has_no_correlation_by_default() {
        let event = Event::new("heartbeat", "agent-1", json!(null));
        assert!(event.correlation_id.is_none());
    }

    #[test]
    fn event_serialization_roundtrip() {
        let event = Event::new("started", "mini", json!({"pid": 1234}));
        let json_str = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.event_type, "started");
        assert_eq!(deserialized.source, "mini");
    }

    // ChannelMessage tests

    #[test]
    fn channel_message_generates_id() {
        let msg = ChannelMessage::new("alice", "hello", "general");
        assert!(msg.message_id.starts_with("general-"));
        assert!(msg.timestamp > 0.0);
    }

    #[test]
    fn channel_message_serialization_roundtrip() {
        let msg = ChannelMessage::new("bob", "world", "dev");
        let json_str = serde_json::to_string(&msg).unwrap();
        let deserialized: ChannelMessage = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.sender, "bob");
        assert_eq!(deserialized.content, "world");
        assert_eq!(deserialized.channel, "dev");
    }

    #[test]
    fn channel_message_metadata_none_skipped_in_json() {
        let msg = ChannelMessage::new("alice", "hi", "ch");
        let json_str = serde_json::to_string(&msg).unwrap();
        assert!(!json_str.contains("metadata"));
    }

    #[test]
    fn channel_message_with_metadata() {
        let msg = ChannelMessage::with_metadata("alice", "hi", "ch", json!({"priority": "high"}));
        assert!(msg.metadata.is_some());
        let json_str = serde_json::to_string(&msg).unwrap();
        assert!(json_str.contains("priority"));
    }

    // NatsConfig tests

    #[test]
    fn nats_config_defaults() {
        let config = NatsConfig::default();
        assert_eq!(config.url, "nats://localhost:4222");
        assert_eq!(config.connect_timeout, Duration::from_secs(5));
        assert_eq!(config.request_timeout, Duration::from_secs(10));
    }

    #[test]
    fn nats_config_custom_url() {
        let config = NatsConfig::new("nats://192.168.8.110:4222");
        assert_eq!(config.url, "nats://192.168.8.110:4222");
    }

    #[test]
    fn nats_config_builder_pattern() {
        let config = NatsConfig::new("nats://localhost:4222")
            .with_connect_timeout(Duration::from_secs(30))
            .with_request_timeout(Duration::from_secs(60));
        assert_eq!(config.connect_timeout, Duration::from_secs(30));
        assert_eq!(config.request_timeout, Duration::from_secs(60));
    }

    #[test]
    fn nats_config_serialization_roundtrip() {
        let config = NatsConfig::new("nats://example.com:4222");
        let json_str = serde_json::to_string(&config).unwrap();
        let deserialized: NatsConfig = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.url, "nats://example.com:4222");
    }

    // StreamConfig tests

    #[test]
    fn stream_config_defaults() {
        let config = StreamConfig::new("EVENTS", vec!["events.>".to_string()]);
        assert_eq!(config.max_age_secs, 86400);
        assert_eq!(config.max_msgs, 100_000);
        assert_eq!(config.storage, "file");
    }

    #[test]
    fn stream_config_builder() {
        let config = StreamConfig::new("TEST", vec!["test.>".to_string()])
            .with_max_age(3600)
            .with_max_msgs(1000)
            .with_memory_storage();
        assert_eq!(config.max_age_secs, 3600);
        assert_eq!(config.max_msgs, 1000);
        assert_eq!(config.storage, "memory");
    }

    // KvBucketConfig tests

    #[test]
    fn kv_bucket_config_defaults() {
        let config = KvBucketConfig::new("test_bucket");
        assert_eq!(config.bucket, "test_bucket");
        assert_eq!(config.history, 5);
        assert!(config.ttl.is_none());
    }

    #[test]
    fn kv_bucket_config_with_ttl() {
        let config = KvBucketConfig::new("status").with_ttl_secs(300);
        assert_eq!(config.ttl, Some(Duration::from_secs(300)));
    }

    #[test]
    fn kv_bucket_config_with_history() {
        let config = KvBucketConfig::new("status").with_history(10);
        assert_eq!(config.history, 10);
    }

    // Error tests

    #[test]
    fn error_display_connection() {
        let err = Error::Connection("refused".to_string());
        assert_eq!(err.to_string(), "connection error: refused");
    }

    #[test]
    fn error_display_not_connected() {
        let err = Error::NotConnected;
        assert_eq!(err.to_string(), "not connected to NATS server");
    }

    #[test]
    fn error_display_key_not_found() {
        let err = Error::KeyNotFound("my_key".to_string());
        assert_eq!(err.to_string(), "key not found: my_key");
    }

    #[test]
    fn error_display_timeout() {
        let err = Error::Timeout(Duration::from_secs(5));
        assert_eq!(err.to_string(), "operation timed out after 5s");
    }

    #[test]
    fn error_from_serde_json() {
        let result: std::result::Result<serde_json::Value, _> = serde_json::from_str("not json");
        let err: Error = result.unwrap_err().into();
        matches!(err, Error::Serialization(_));
    }
}
