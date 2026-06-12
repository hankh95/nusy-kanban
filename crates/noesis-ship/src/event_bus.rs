//! JetStream event publishing and subscription.
//!
//! Durable event bus with persistence, replay, and pattern-based filtering.
//! The nervous system — all significant actions emit events here.
//!
//! Default stream: `SHIP_EVENTS`, subjects: `ship.events.>`, 24h retention, 100k max messages.

use crate::connection::ConnectionManager;
use crate::types::{Error, NatsConfig, Result, ShipEvent, StreamConfig};
use futures::StreamExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Default event bus stream configuration.
pub fn default_stream_config() -> StreamConfig {
    StreamConfig::new("SHIP_EVENTS", vec!["ship.events.>".to_string()])
}

/// Type alias for async ShipEvent handlers.
pub type ShipEventHandler =
    Box<dyn Fn(ShipEvent) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// JetStream-backed event bus with durable persistence and replay.
pub struct EventBus {
    config: NatsConfig,
    stream_config: StreamConfig,
    subject_prefix: String,
    conn: Arc<Mutex<ConnectionManager>>,
    source: String,
}

impl EventBus {
    /// Create a new EventBus with default stream config.
    pub fn new(config: NatsConfig) -> Self {
        Self::with_stream(config, default_stream_config(), "ship.events")
    }

    /// Create an EventBus with custom stream config and subject prefix.
    pub fn with_stream(
        config: NatsConfig,
        stream_config: StreamConfig,
        subject_prefix: impl Into<String>,
    ) -> Self {
        Self {
            conn: Arc::new(Mutex::new(ConnectionManager::new(config.clone()))),
            config,
            stream_config,
            subject_prefix: subject_prefix.into(),
            source: String::new(),
        }
    }

    /// Set the source identifier for events emitted by this bus.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Connect to NATS and ensure the JetStream stream exists.
    pub async fn connect(&self) -> Result<()> {
        let mut conn = self.conn.lock().await;
        conn.connect().await?;
        conn.ensure_stream(&self.stream_config).await?;
        Ok(())
    }

    /// Disconnect from NATS.
    pub async fn disconnect(&self) -> Result<()> {
        self.conn.lock().await.disconnect().await
    }

    /// Whether the bus is connected.
    pub async fn is_connected(&self) -> bool {
        self.conn.lock().await.is_connected()
    }

    /// Emit an event to the bus.
    ///
    /// The event is wrapped in a [`ShipEvent`] envelope with auto-generated
    /// timestamp and correlation ID, then published to JetStream for durability.
    pub async fn emit(
        &self,
        event_type: &str,
        source: &str,
        payload: serde_json::Value,
        correlation_id: Option<&str>,
    ) -> Result<()> {
        let subject = format!("{}.{}", self.subject_prefix, event_type);
        let event = match correlation_id {
            Some(cid) => ShipEvent::with_correlation(event_type, source, payload, cid),
            None => ShipEvent::new(event_type, source, payload),
        };
        let data = serde_json::to_vec(&event)?;

        let conn = self.conn.lock().await;
        let js = conn.jetstream()?;
        js.publish(subject, data.into())
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;

        Ok(())
    }

    /// Convenience: emit using the bus's configured source.
    pub async fn emit_event(&self, event_type: &str, payload: serde_json::Value) -> Result<()> {
        self.emit(event_type, &self.source, payload, None).await
    }

    /// Subscribe to events matching a pattern with a durable consumer.
    ///
    /// The `consumer_name` must be unique per subscriber. Pattern supports
    /// NATS wildcards: `*` (single token), `>` (multi-token).
    pub async fn subscribe(
        &self,
        pattern: &str,
        consumer_name: &str,
        handler: impl Fn(ShipEvent) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let full_pattern = format!("{}.{}", self.subject_prefix, pattern);
        let conn = self.conn.lock().await;
        let js = conn.jetstream()?;

        let consumer = js
            .create_consumer_on_stream(
                async_nats::jetstream::consumer::push::Config {
                    filter_subject: full_pattern,
                    durable_name: Some(consumer_name.to_string()),
                    deliver_subject: format!("_deliver.{}.{}", consumer_name, uuid::Uuid::new_v4()),
                    ..Default::default()
                },
                &self.stream_config.name,
            )
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;

        let handler = Arc::new(handler);

        let handle = tokio::spawn(async move {
            let mut messages = match consumer.messages().await {
                Ok(m) => m,
                Err(_) => return,
            };

            while let Some(Ok(msg)) = messages.next().await {
                if let Ok(event) = serde_json::from_slice::<ShipEvent>(&msg.payload) {
                    handler(event).await;
                }
                let _ = msg.ack().await;
            }
        });

        Ok(handle)
    }

    /// Get the NATS config (for creating additional connections).
    pub fn config(&self) -> &NatsConfig {
        &self.config
    }

    /// Get the stream config.
    pub fn stream_config(&self) -> &StreamConfig {
        &self.stream_config
    }
}

// ─── Global singleton ───────────────────────────────────────────────────────

static GLOBAL_EVENT_BUS: tokio::sync::OnceCell<EventBus> = tokio::sync::OnceCell::const_new();

/// Initialize the global event bus singleton.
///
/// Must be called once before [`emit_event`]. Panics if called twice.
pub async fn init_global_event_bus(config: NatsConfig) -> Result<()> {
    let bus = EventBus::new(config).with_source("global");
    bus.connect().await?;
    GLOBAL_EVENT_BUS
        .set(bus)
        .map_err(|_| Error::Connection("global event bus already initialized".into()))
}

/// Emit an event via the global singleton.
///
/// Returns an error if [`init_global_event_bus`] hasn't been called.
pub async fn emit_event(event_type: &str, source: &str, payload: serde_json::Value) -> Result<()> {
    let bus = GLOBAL_EVENT_BUS.get().ok_or(Error::NotConnected)?;
    bus.emit(event_type, source, payload, None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_stream_config_values() {
        let config = default_stream_config();
        assert_eq!(config.name, "SHIP_EVENTS");
        assert_eq!(config.subjects, vec!["ship.events.>"]);
        assert_eq!(config.max_age_secs, 86400); // 24h
        assert_eq!(config.max_msgs, 100_000);
        assert_eq!(config.storage, "file");
    }

    #[test]
    fn event_bus_new_defaults() {
        let bus = EventBus::new(NatsConfig::default());
        assert_eq!(bus.subject_prefix, "ship.events");
        assert_eq!(bus.stream_config.name, "SHIP_EVENTS");
    }

    #[test]
    fn event_bus_with_custom_stream() {
        let stream = StreamConfig::new("CUSTOM", vec!["custom.>".to_string()])
            .with_max_age(3600)
            .with_memory_storage();
        let bus = EventBus::with_stream(NatsConfig::default(), stream, "custom");
        assert_eq!(bus.subject_prefix, "custom");
        assert_eq!(bus.stream_config.name, "CUSTOM");
        assert_eq!(bus.stream_config.max_age_secs, 3600);
    }

    #[test]
    fn event_bus_with_source() {
        let bus = EventBus::new(NatsConfig::default()).with_source("kanban-server");
        assert_eq!(bus.source, "kanban-server");
    }

    #[test]
    fn ship_event_envelope_subject_format() {
        // Verify the subject format would be correct
        let prefix = "ship.events";
        let event_type = "item.created";
        let subject = format!("{}.{}", prefix, event_type);
        assert_eq!(subject, "ship.events.item.created");
    }

    #[test]
    fn ship_event_envelope_creation() {
        let event = ShipEvent::new("item.created", "kanban-server", json!({"id": "EX-3001"}));
        assert_eq!(event.event_type, "item.created");
        assert_eq!(event.source, "kanban-server");
        assert_eq!(event.version, "1.0");
        assert_eq!(event.correlation_id.len(), 8);
    }

    #[test]
    fn ship_event_with_explicit_correlation() {
        let event = ShipEvent::with_correlation(
            "item.moved",
            "server",
            json!({"from": "backlog", "to": "in_progress"}),
            "abc12345",
        );
        assert_eq!(event.correlation_id, "abc12345");
    }
}
