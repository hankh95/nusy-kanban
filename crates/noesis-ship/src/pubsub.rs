//! Raw NATS publish/subscribe.
//!
//! Fire-and-forget messaging using NATS Core. No persistence, no JetStream.
//!
//! Three components:
//! - [`Publisher`] — publish events to subjects
//! - [`Subscriber`] — subscribe to subjects with async handlers
//! - [`PubSub`] — combined publisher + subscriber (maps to Python ShipEventBus)

use crate::connection::ConnectionManager;
use crate::types::{Error, Event, NatsConfig, Result};
use futures::StreamExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Generic event type for PubSub subjects.
///
/// NOT NuSy-specific — these are generic event categories.
#[derive(Debug, Clone)]
pub enum EventType {
    /// Custom event type with arbitrary name.
    Custom(String),
    /// Periodic heartbeat signal.
    Heartbeat,
    /// Service/agent started.
    Started,
    /// Service/agent stopped.
    Stopped,
    /// Error occurrence.
    Error,
}

impl EventType {
    /// Get the subject suffix for this event type.
    pub fn subject(&self) -> &str {
        match self {
            EventType::Custom(s) => s,
            EventType::Heartbeat => "heartbeat",
            EventType::Started => "started",
            EventType::Stopped => "stopped",
            EventType::Error => "error",
        }
    }

    /// Create a subject with an entity prefix: `{entity}.{event_type}`.
    pub fn with_entity(&self, entity_id: &str) -> String {
        format!("{}.{}", entity_id, self.subject())
    }
}

/// Type alias for async event handlers.
pub type EventHandler =
    Box<dyn Fn(Event) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// NATS Core publisher — fire-and-forget event publishing.
pub struct Publisher {
    source_id: String,
    conn: ConnectionManager,
}

impl Publisher {
    /// Create a new Publisher (not yet connected).
    pub fn new(source_id: impl Into<String>, config: NatsConfig) -> Self {
        Self {
            source_id: source_id.into(),
            conn: ConnectionManager::new(config),
        }
    }

    /// Connect to NATS.
    pub async fn connect(&mut self) -> Result<()> {
        self.conn.connect().await
    }

    /// Disconnect from NATS.
    pub async fn disconnect(&mut self) -> Result<()> {
        self.conn.disconnect().await
    }

    /// Publish raw bytes to a subject.
    pub async fn publish(&self, subject: &str, payload: &[u8]) -> Result<()> {
        let client = self.conn.client()?;
        client
            .publish(subject.to_string(), payload.to_vec().into())
            .await
            .map_err(|e| Error::Connection(e.to_string()))
    }

    /// Emit a typed event to a subject derived from the event type.
    pub async fn emit(&self, event_type: &EventType, payload: serde_json::Value) -> Result<()> {
        let event = Event::new(event_type.subject(), &self.source_id, payload);
        let data = serde_json::to_vec(&event)?;
        self.publish(event_type.subject(), &data).await
    }

    /// Emit an event scoped to a specific entity (e.g., a being).
    pub async fn emit_being(
        &self,
        being_id: &str,
        event_type: &EventType,
        payload: serde_json::Value,
    ) -> Result<()> {
        let subject = event_type.with_entity(being_id);
        let event = Event::new(event_type.subject(), &self.source_id, payload);
        let data = serde_json::to_vec(&event)?;
        self.publish(&subject, &data).await
    }
}

/// NATS Core subscriber — subscribe to subjects with async handlers.
pub struct Subscriber {
    subscriber_id: String,
    conn: ConnectionManager,
    handlers: Vec<(String, Arc<EventHandler>)>,
    shutdown: tokio::sync::watch::Sender<bool>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

impl Subscriber {
    /// Create a new Subscriber (not yet connected).
    pub fn new(subscriber_id: impl Into<String>, config: NatsConfig) -> Self {
        let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        Self {
            subscriber_id: subscriber_id.into(),
            conn: ConnectionManager::new(config),
            handlers: Vec::new(),
            shutdown,
            shutdown_rx,
        }
    }

    /// Connect to NATS.
    pub async fn connect(&mut self) -> Result<()> {
        self.conn.connect().await
    }

    /// Disconnect from NATS.
    pub async fn disconnect(&mut self) -> Result<()> {
        let _ = self.shutdown.send(true);
        self.conn.disconnect().await
    }

    /// Register a handler for a subject pattern.
    ///
    /// Patterns support NATS wildcards: `*` (single token), `>` (multi-token).
    pub fn subscribe<F, Fut>(&mut self, subject: impl Into<String>, handler: F)
    where
        F: Fn(Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handler: EventHandler = Box::new(move |event| Box::pin(handler(event)));
        self.handlers.push((subject.into(), Arc::new(handler)));
    }

    /// Run the subscriber, blocking until shutdown.
    ///
    /// Spawns a tokio task per subscription.
    pub async fn run(&self) -> Result<()> {
        let client = self.conn.client()?;

        let mut tasks = Vec::new();
        for (subject, handler) in &self.handlers {
            let mut sub = client
                .subscribe(subject.clone())
                .await
                .map_err(|e| Error::Connection(e.to_string()))?;

            let handler = Arc::clone(handler);
            let mut shutdown_rx = self.shutdown_rx.clone();
            let _sub_id = self.subscriber_id.clone();

            tasks.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        msg = sub.next() => {
                            if let Some(msg) = msg {
                                if let Ok(event) = serde_json::from_slice::<Event>(&msg.payload) {
                                    handler(event).await;
                                }
                            } else {
                                break;
                            }
                        }
                        _ = shutdown_rx.changed() => {
                            break;
                        }
                    }
                }
            }));
        }

        // Wait for all tasks
        for task in tasks {
            let _ = task.await;
        }
        Ok(())
    }
}

/// Combined publisher + subscriber (maps to Python ShipEventBus).
pub struct PubSub {
    source_id: String,
    conn: Arc<Mutex<ConnectionManager>>,
    handlers: Vec<(String, Arc<EventHandler>)>,
    shutdown: tokio::sync::watch::Sender<bool>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

impl PubSub {
    /// Create a new PubSub bus (not yet connected).
    pub fn new(bus_id: impl Into<String>, config: NatsConfig) -> Self {
        let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        Self {
            source_id: bus_id.into(),
            conn: Arc::new(Mutex::new(ConnectionManager::new(config))),
            handlers: Vec::new(),
            shutdown,
            shutdown_rx,
        }
    }

    /// Connect to NATS.
    pub async fn connect(&self) -> Result<()> {
        self.conn.lock().await.connect().await
    }

    /// Register a handler for a subject pattern.
    pub fn subscribe<F, Fut>(&mut self, subject: impl Into<String>, handler: F)
    where
        F: Fn(Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handler: EventHandler = Box::new(move |event| Box::pin(handler(event)));
        self.handlers.push((subject.into(), Arc::new(handler)));
    }

    /// Emit a typed event.
    pub async fn emit(&self, event_type: &EventType, payload: serde_json::Value) -> Result<()> {
        let event = Event::new(event_type.subject(), &self.source_id, payload);
        let data = serde_json::to_vec(&event)?;
        let conn = self.conn.lock().await;
        let client = conn.client()?;
        client
            .publish(event_type.subject().to_string(), data.into())
            .await
            .map_err(|e| Error::Connection(e.to_string()))
    }

    /// Publish raw bytes to a subject.
    pub async fn publish(&self, subject: &str, payload: &[u8]) -> Result<()> {
        let conn = self.conn.lock().await;
        let client = conn.client()?;
        client
            .publish(subject.to_string(), payload.to_vec().into())
            .await
            .map_err(|e| Error::Connection(e.to_string()))
    }

    /// Run the subscriber side, blocking until shutdown.
    pub async fn run(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        let client = conn.client()?.clone();
        drop(conn);

        let mut tasks = Vec::new();
        for (subject, handler) in &self.handlers {
            let mut sub = client
                .subscribe(subject.clone())
                .await
                .map_err(|e| Error::Connection(e.to_string()))?;

            let handler = Arc::clone(handler);
            let mut shutdown_rx = self.shutdown_rx.clone();

            tasks.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        msg = sub.next() => {
                            if let Some(msg) = msg {
                                if let Ok(event) = serde_json::from_slice::<Event>(&msg.payload) {
                                    handler(event).await;
                                }
                            } else {
                                break;
                            }
                        }
                        _ = shutdown_rx.changed() => {
                            break;
                        }
                    }
                }
            }));
        }

        for task in tasks {
            let _ = task.await;
        }
        Ok(())
    }

    /// Signal shutdown for the run loop.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_subject() {
        assert_eq!(EventType::Heartbeat.subject(), "heartbeat");
        assert_eq!(EventType::Started.subject(), "started");
        assert_eq!(EventType::Stopped.subject(), "stopped");
        assert_eq!(EventType::Error.subject(), "error");
        assert_eq!(EventType::Custom("my.event".into()).subject(), "my.event");
    }

    #[test]
    fn event_type_with_entity() {
        assert_eq!(
            EventType::Heartbeat.with_entity("being-1"),
            "being-1.heartbeat"
        );
        assert_eq!(
            EventType::Custom("status".into()).with_entity("mini"),
            "mini.status"
        );
    }

    #[test]
    fn publisher_new_not_connected() {
        let pub_ = Publisher::new("test", NatsConfig::default());
        assert!(!pub_.conn.is_connected());
    }

    #[test]
    fn subscriber_new_not_connected() {
        let sub = Subscriber::new("test", NatsConfig::default());
        assert!(!sub.conn.is_connected());
    }

    #[test]
    fn pubsub_new() {
        let _bus = PubSub::new("test-bus", NatsConfig::default());
    }

    #[test]
    fn subscriber_register_handler() {
        let mut sub = Subscriber::new("test", NatsConfig::default());
        sub.subscribe("test.>", |_event| async {});
        assert_eq!(sub.handlers.len(), 1);
    }

    #[test]
    fn pubsub_register_handler() {
        let mut bus = PubSub::new("test", NatsConfig::default());
        bus.subscribe("events.>", |_event| async {});
        assert_eq!(bus.handlers.len(), 1);
    }
}
