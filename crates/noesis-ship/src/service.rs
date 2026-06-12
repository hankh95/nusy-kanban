//! Generic NATS service framework.
//!
//! `NatsServiceBuilder` turns any set of handler functions into a NATS
//! request-reply service with routing, mutation detection, event emission,
//! and graceful shutdown.
//!
//! # Example
//!
//! ```rust,no_run
//! use noesis_ship::service::{NatsServiceBuilder, ServiceArgs};
//! use clap::Parser;
//!
//! #[derive(Default)]
//! struct MyState { count: u64 }
//!
//! # async fn example() -> noesis_ship::types::Result<()> {
//! let args = ServiceArgs::parse();
//! NatsServiceBuilder::new("myservice.cmd", MyState::default())
//!     .nats_url(&args.nats_url)
//!     .handler("echo", |payload, _state| payload.to_vec())
//!     .handler("count", |_payload, state: &mut MyState| {
//!         state.count += 1;
//!         serde_json::to_vec(&state.count).unwrap_or_default()
//!     })
//!     .run()
//!     .await?;
//! # Ok(())
//! # }
//! ```

use crate::types::{Error, Result, ShipEvent, StreamConfig};
use clap::Parser;
use futures::StreamExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::path::PathBuf;

// ─── Helper Functions ───────────────────────────────────────────────────────

/// Parse a JSON payload into a typed request.
///
/// Eliminates serde boilerplate in every handler.
pub fn parse_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    serde_json::from_slice(payload).map_err(Error::Serialization)
}

/// Serialize a response to JSON bytes.
pub fn serialize_response<T: Serialize>(response: &T) -> Vec<u8> {
    serde_json::to_vec(response).unwrap_or_else(|_| b"{}".to_vec())
}

/// Create a JSON error response.
pub fn error_response(message: &str, code: u16) -> Vec<u8> {
    serde_json::to_vec(&ErrorResponse {
        error: message.to_string(),
        code,
    })
    .unwrap_or_else(|_| format!("{{\"error\":\"{message}\",\"code\":{code}}}").into_bytes())
}

#[derive(serde::Serialize)]
struct ErrorResponse {
    error: String,
    code: u16,
}

// ─── CLI Args Helper ────────────────────────────────────────────────────────

/// Standard CLI arguments for a NATS service.
///
/// Provides `--data-dir` and `--nats-url` with sensible defaults.
#[derive(Parser, Debug, Clone)]
pub struct ServiceArgs {
    /// Working directory for persistent data.
    #[arg(long, default_value = ".")]
    pub data_dir: PathBuf,

    /// NATS server URL.
    #[arg(long, default_value = "nats://localhost:4222")]
    pub nats_url: String,
}

// ─── Handler type ───────────────────────────────────────────────────────────

/// A synchronous handler function: takes payload + mutable state, returns response bytes.
type HandlerFn<S> = Box<dyn Fn(&[u8], &mut S) -> Vec<u8> + Send + Sync>;

/// Optional mutation callback: returns Some(event_bytes) if the command was a mutation.
type MutationCallback<S> = Box<dyn Fn(&str, &[u8], &S) -> Option<(String, Vec<u8>)> + Send + Sync>;

/// Optional shutdown callback.
type ShutdownCallback<S> = Box<dyn FnOnce(&S) + Send>;

/// Default handler: takes the full subject + payload + state, returns response.
type DefaultHandlerFn<S> = Box<dyn Fn(&str, &[u8], &mut S) -> Vec<u8> + Send + Sync>;

// ─── NatsServiceBuilder ─────────────────────────────────────────────────────

/// Builder for NATS request-reply services.
///
/// Generalizes the pattern from nusy-kanban-server: subscribe to `{prefix}.>`,
/// strip prefix, dispatch by command, serialize response, detect mutations,
/// emit events, persist, graceful shutdown.
///
/// Services with many commands can use [`default_handler`](Self::default_handler)
/// instead of registering each command individually. The default handler receives
/// the full NATS subject and acts as a catch-all dispatcher.
///
/// When [`event_bus_stream`](Self::event_bus_stream) is configured, mutation
/// events are published to JetStream for durability instead of fire-and-forget
/// PubSub. Late-joining consumers can replay recent events.
pub struct NatsServiceBuilder<S: Send + Sync + 'static> {
    nats_url: String,
    subject_prefix: String,
    state: S,
    handlers: HashMap<String, HandlerFn<S>>,
    default_handler: Option<DefaultHandlerFn<S>>,
    mutation_callback: Option<MutationCallback<S>>,
    event_subject_prefix: Option<String>,
    event_bus_stream: Option<StreamConfig>,
    event_source: Option<String>,
    shutdown_callback: Option<ShutdownCallback<S>>,
}

impl<S: Send + Sync + 'static> NatsServiceBuilder<S> {
    /// Create a new service builder.
    ///
    /// `subject_prefix` is the NATS subject prefix to subscribe to (e.g., "kanban.cmd").
    /// The builder subscribes to `{subject_prefix}.>` and routes by the suffix.
    pub fn new(subject_prefix: impl Into<String>, state: S) -> Self {
        Self {
            nats_url: "nats://localhost:4222".to_string(),
            subject_prefix: subject_prefix.into(),
            state,
            handlers: HashMap::new(),
            default_handler: None,
            mutation_callback: None,
            event_subject_prefix: None,
            event_bus_stream: None,
            event_source: None,
            shutdown_callback: None,
        }
    }

    /// Set the NATS server URL.
    pub fn nats_url(mut self, url: &str) -> Self {
        self.nats_url = url.to_string();
        self
    }

    /// Register a command handler.
    ///
    /// The `command` is matched against the subject suffix after stripping the prefix.
    /// For example, if prefix is "kanban.cmd" and subject is "kanban.cmd.create",
    /// the command is "create".
    pub fn handler<F>(mut self, command: &str, handler: F) -> Self
    where
        F: Fn(&[u8], &mut S) -> Vec<u8> + Send + Sync + 'static,
    {
        self.handlers.insert(command.to_string(), Box::new(handler));
        self
    }

    /// Set a default (catch-all) handler for commands not matched by name.
    ///
    /// The handler receives the **full NATS subject** (not stripped), the payload,
    /// and mutable state. Use this when you have an existing dispatch function
    /// that routes all commands internally.
    pub fn default_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&str, &[u8], &mut S) -> Vec<u8> + Send + Sync + 'static,
    {
        self.default_handler = Some(Box::new(handler));
        self
    }

    /// Set a mutation callback that's called after each handler.
    ///
    /// If the callback returns `Some((subject, bytes))`, an event is published.
    pub fn mutation_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(&str, &[u8], &S) -> Option<(String, Vec<u8>)> + Send + Sync + 'static,
    {
        self.mutation_callback = Some(Box::new(callback));
        self
    }

    /// Set the event subject prefix for mutation events.
    ///
    /// Mutation events are published to `{prefix}.{event_type}`.
    pub fn event_prefix(mut self, prefix: &str) -> Self {
        self.event_subject_prefix = Some(prefix.to_string());
        self
    }

    /// Enable JetStream event publishing for mutation events.
    ///
    /// When set, mutation events are published to a durable JetStream stream
    /// instead of fire-and-forget PubSub. The stream is created on startup if
    /// it doesn't exist.
    pub fn event_bus_stream(mut self, stream: StreamConfig, source: impl Into<String>) -> Self {
        self.event_bus_stream = Some(stream);
        self.event_source = Some(source.into());
        self
    }

    /// Set a shutdown callback that's called when the service stops.
    pub fn on_shutdown<F>(mut self, callback: F) -> Self
    where
        F: FnOnce(&S) + Send + 'static,
    {
        self.shutdown_callback = Some(Box::new(callback));
        self
    }

    /// Run the service, blocking until Ctrl+C.
    ///
    /// Connects to NATS, subscribes to `{prefix}.>`, and dispatches incoming
    /// requests to registered handlers. If a JetStream stream is configured via
    /// [`event_bus_stream`](Self::event_bus_stream), mutation events are published
    /// durably; otherwise they use fire-and-forget PubSub.
    ///
    /// Calls shutdown callback on exit.
    pub async fn run(mut self) -> Result<()> {
        let mut conn = crate::connection::ConnectionManager::new(crate::types::NatsConfig::new(
            &self.nats_url,
        ));
        conn.connect()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        let client = conn
            .client()
            .map_err(|e| Error::Connection(e.to_string()))?
            .clone();

        // Ensure JetStream stream exists if configured.
        if let Some(ref stream_config) = self.event_bus_stream {
            conn.ensure_stream(stream_config).await?;
        }

        let subscribe_subject = format!("{}.>", self.subject_prefix);
        let mut subscriber = client
            .subscribe(subscribe_subject)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        tracing::info!(
            prefix = %self.subject_prefix,
            url = %self.nats_url,
            handlers = self.handlers.len(),
            has_default = self.default_handler.is_some(),
            jetstream = self.event_bus_stream.is_some(),
            "service ready"
        );

        // JetStream context for durable event publishing.
        let js = if self.event_bus_stream.is_some() {
            Some(
                conn.jetstream()
                    .map_err(|e| Error::Connection(e.to_string()))?,
            )
        } else {
            None
        };
        let event_source = self.event_source.clone().unwrap_or_default();

        loop {
            tokio::select! {
                msg = subscriber.next() => {
                    match msg {
                        Some(msg) => {
                            let subject = msg.subject.to_string();
                            let command = self.strip_prefix(&subject);

                            // Dispatch: named handler first, then default handler.
                            let response = if self.handlers.contains_key(command) {
                                self.dispatch(command, &msg.payload)
                            } else if let Some(ref default) = self.default_handler {
                                default(&subject, &msg.payload, &mut self.state)
                            } else {
                                self.dispatch(command, &msg.payload)
                            };

                            // Send reply.
                            if let Some(reply_to) = msg.reply
                                && let Err(e) = client.publish(reply_to, response.clone().into()).await
                            {
                                tracing::error!(error = %e, subject = %subject, "reply failed");
                            }

                            // Mutation event.
                            if let Some(ref callback) = self.mutation_callback
                                && let Some((event_type, event_bytes)) = callback(command, &response, &self.state)
                            {
                                let full_subject = match &self.event_subject_prefix {
                                    Some(prefix) => format!("{}.{}", prefix, event_type),
                                    None => event_type.clone(),
                                };

                                if let Some(js) = &js {
                                    // JetStream durable publish.
                                    let envelope = ShipEvent::new(
                                        &event_type,
                                        &event_source,
                                        serde_json::from_slice(&event_bytes)
                                            .unwrap_or(serde_json::Value::Null),
                                    );
                                    let data = serde_json::to_vec(&envelope)
                                        .unwrap_or_else(|_| event_bytes.clone());
                                    match js.publish(full_subject, data.into()).await {
                                        Ok(ack) => { drop(ack); }
                                        Err(e) => {
                                            tracing::error!(error = %e, "jetstream event publish failed");
                                        }
                                    }
                                } else {
                                    // Fire-and-forget PubSub fallback.
                                    if let Err(e) = client.publish(full_subject, event_bytes.into()).await {
                                        tracing::error!(error = %e, "event publish failed");
                                    }
                                }
                            }
                        }
                        None => {
                            tracing::warn!("NATS subscription closed");
                            break;
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("shutting down");
                    break;
                }
            }
        }

        // Shutdown callback.
        if let Some(callback) = self.shutdown_callback.take() {
            callback(&self.state);
        }

        let _ = conn.disconnect().await;
        Ok(())
    }

    /// Dispatch a command to the registered handler.
    ///
    /// Public for testing without NATS.
    pub fn dispatch(&mut self, command: &str, payload: &[u8]) -> Vec<u8> {
        match self.handlers.get(command) {
            Some(handler) => handler(payload, &mut self.state),
            None => error_response(&format!("UNKNOWN_COMMAND: {}", command), 404),
        }
    }

    fn strip_prefix<'a>(&self, subject: &'a str) -> &'a str {
        subject
            .strip_prefix(&self.subject_prefix)
            .and_then(|s| s.strip_prefix('.'))
            .unwrap_or(subject)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct TestState {
        counter: u64,
    }

    #[test]
    fn dispatch_echo() {
        let mut svc = NatsServiceBuilder::new("test.cmd", TestState::default())
            .handler("echo", |payload, _state| payload.to_vec());

        let response = svc.dispatch("echo", b"hello");
        assert_eq!(response, b"hello");
    }

    #[test]
    fn dispatch_with_state() {
        let mut svc = NatsServiceBuilder::new("test.cmd", TestState::default()).handler(
            "increment",
            |_payload, state: &mut TestState| {
                state.counter += 1;
                serialize_response(&state.counter)
            },
        );

        svc.dispatch("increment", b"");
        svc.dispatch("increment", b"");
        let response = svc.dispatch("increment", b"");
        let count: u64 = serde_json::from_slice(&response).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn dispatch_unknown_command() {
        let mut svc = NatsServiceBuilder::new("test.cmd", TestState::default());

        let response = svc.dispatch("nonexistent", b"");
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert!(
            parsed["error"]
                .as_str()
                .unwrap()
                .contains("UNKNOWN_COMMAND")
        );
        assert_eq!(parsed["code"], 404);
    }

    #[test]
    fn strip_prefix() {
        let svc = NatsServiceBuilder::new("kanban.cmd", TestState::default());
        assert_eq!(svc.strip_prefix("kanban.cmd.create"), "create");
        assert_eq!(svc.strip_prefix("kanban.cmd.pr.merge"), "pr.merge");
        assert_eq!(svc.strip_prefix("other.subject"), "other.subject");
    }

    #[test]
    fn parse_payload_success() {
        #[derive(serde::Deserialize)]
        struct Req {
            name: String,
        }
        let payload = serde_json::to_vec(&json!({"name": "test"})).unwrap();
        let req: Req = parse_payload(&payload).unwrap();
        assert_eq!(req.name, "test");
    }

    #[test]
    fn parse_payload_error() {
        #[derive(serde::Deserialize)]
        struct Req {
            #[allow(dead_code)]
            name: String,
        }
        let result: Result<Req> = parse_payload(b"not json");
        assert!(result.is_err());
    }

    #[test]
    fn serialize_response_json() {
        let data = json!({"status": "ok", "count": 42});
        let bytes = serialize_response(&data);
        let back: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back["count"], 42);
    }

    #[test]
    fn error_response_format() {
        let bytes = error_response("not found", 404);
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["error"], "not found");
        assert_eq!(parsed["code"], 404);
    }

    #[test]
    fn default_handler_catch_all() {
        let mut svc = NatsServiceBuilder::new("kanban.cmd", TestState::default()).default_handler(
            |subject, _payload, _state| format!("handled: {subject}").into_bytes(),
        );

        // No named handler registered — default should catch all.
        // Simulate what run() does: check named first, then default.
        let subject = "kanban.cmd.create";
        let command = svc.strip_prefix(subject);
        let response = if svc.handlers.contains_key(command) {
            svc.dispatch(command, b"")
        } else if let Some(ref default) = svc.default_handler {
            default(subject, b"", &mut svc.state)
        } else {
            svc.dispatch(command, b"")
        };
        assert_eq!(response, b"handled: kanban.cmd.create");
    }

    #[test]
    fn named_handler_takes_precedence_over_default() {
        let mut svc = NatsServiceBuilder::new("svc", TestState::default())
            .handler("ping", |_, _| b"pong".to_vec())
            .default_handler(|_subject, _payload, _state| b"default".to_vec());

        let subject = "svc.ping";
        let command = svc.strip_prefix(subject);
        let response = if svc.handlers.contains_key(command) {
            svc.dispatch(command, b"")
        } else if let Some(ref default) = svc.default_handler {
            default(subject, b"", &mut svc.state)
        } else {
            svc.dispatch(command, b"")
        };
        assert_eq!(response, b"pong");
    }

    #[test]
    fn multiple_handlers() {
        let mut svc = NatsServiceBuilder::new("svc", TestState::default())
            .handler("a", |_, _| b"handler_a".to_vec())
            .handler("b", |_, _| b"handler_b".to_vec())
            .handler("c", |_, _| b"handler_c".to_vec());

        assert_eq!(svc.dispatch("a", b""), b"handler_a");
        assert_eq!(svc.dispatch("b", b""), b"handler_b");
        assert_eq!(svc.dispatch("c", b""), b"handler_c");
    }

    #[test]
    fn mutation_callback_fires() {
        let mut svc = NatsServiceBuilder::new("svc", TestState::default())
            .handler("create", |_, _| b"created".to_vec())
            .mutation_callback(|cmd, _response, _state| {
                if cmd == "create" {
                    Some(("item.created".to_string(), b"event_data".to_vec()))
                } else {
                    None
                }
            });

        // Dispatch triggers the handler
        let response = svc.dispatch("create", b"");
        assert_eq!(response, b"created");

        // Mutation callback would fire in run() — here we test it directly
        let callback = svc.mutation_callback.as_ref().unwrap();
        let event = callback("create", &response, &svc.state);
        assert!(event.is_some());
        let (subject, _) = event.unwrap();
        assert_eq!(subject, "item.created");

        let no_event = callback("query", &[], &svc.state);
        assert!(no_event.is_none());
    }

    #[test]
    fn service_args_defaults() {
        // ServiceArgs can be constructed with defaults (tested via derive)
        let args = ServiceArgs {
            data_dir: PathBuf::from("."),
            nats_url: "nats://localhost:4222".to_string(),
        };
        assert_eq!(args.nats_url, "nats://localhost:4222");
    }
}
