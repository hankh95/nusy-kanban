//! # noesis-ship
//!
//! Rust NATS communication platform for multi-agent AI systems.
//!
//! Provides five core primitives over NATS:
//!
//! - **PubSub** — Fire-and-forget publish/subscribe (NATS Core)
//! - **EventBus** — Durable event streaming (JetStream)
//! - **Channels** — Point-to-point messaging with history (JetStream)
//! - **KV Store** — Key-value state management (NATS KV)
//! - **Object Store** — Large blob storage (NATS Object Store)
//! - **JobQueue** — Generic job lifecycle (queued → running → complete/failed)
//!
//! Plus **NatsServiceBuilder** — a framework for building NATS request-reply
//! services in ~20 lines of glue code.
//!
//! ## Quick Start
//!
//! ```rust
//! use noesis_ship::types::NatsConfig;
//!
//! let config = NatsConfig::new("nats://localhost:4222");
//! assert_eq!(config.url, "nats://localhost:4222");
//! ```

pub mod channels;
pub mod connection;
pub mod event_bus;
pub mod job_queue;
pub mod kv;
pub mod object_store;
pub mod pubsub;
pub mod service;
pub mod types;
