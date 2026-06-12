//! nusy-kanban-server — NATS server + client bridge for Arrow-native kanban.
//!
//! Provides a NATS request-reply service that wraps the `nusy-kanban` library.
//! All kanban commands are available via `kanban.cmd.*` subjects, and mutations
//! broadcast events to `kanban.event.*` for real-time consumers like Command Deck.
//!
//! # Architecture
//!
//! ```text
//! NATS Client → kanban.cmd.{command} → Server → nusy-kanban lib → JSON response
//!                                        ↓
//!                              kanban.event.{type} → Command Deck / other consumers
//! ```

pub mod events;
pub mod handlers;
pub mod state;
