//! nusy-conductor — Agent orchestration for the NuSy kanban system.
//!
//! Reads kanban state via NATS (does NOT import nusy-kanban directly),
//! decomposes expeditions into phases, and tracks agent availability.
//!
//! # Architecture
//!
//! The conductor connects to the same NATS server as `nusy-kanban-server`
//! and uses the `kanban.cmd.*` request-reply protocol to query state.
//! It subscribes to `kanban.event.*` for real-time mutation updates.
//!
//! Core modules:
//! - [`reader`] — Kanban state reader via NATS
//! - [`decomposer`] — Expedition phase extraction and progress tracking
//! - [`state`] — Agent assignment and availability tracking
//! - [`ci_runner`] — Synchronous CI check execution (cargo test/clippy/fmt)
//! - [`ci_service`] — NATS CI service (request/reply + result publication)
//! - [`review_cycle`] — PR review cycle automation with CI gating
//! - [`monitor`] — Blocker detection and daily summaries

pub mod ci_runner;
pub mod ci_service;
pub mod decomposer;
pub mod monitor;
pub mod reader;
pub mod review_cycle;
pub mod state;
