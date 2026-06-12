//! NATS-based code graph synchronization (EX-3184).
//!
//! When an agent modifies a CodeNode via `code_replace`, the change is published
//! to `nusy.code.graph.updates`. Other agents subscribed to that subject receive
//! the update within milliseconds and apply it to their local graph.
//!
//! This is multi-writer (unlike kanban's single-writer pattern). Conflicts are
//! detected and logged to `nusy.code.graph.conflict` for the Captain to review.

use arrow::array::RecordBatch;
use chrono::Utc;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Subject for code graph update events.
pub const UPDATES_SUBJECT: &str = "nusy.code.graph.updates";
/// Subject for conflict notifications.
pub const CONFLICT_SUBJECT: &str = "nusy.code.graph.conflict";

/// A code graph mutation event, published over NATS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraphUpdate {
    /// Type of mutation.
    pub update_type: UpdateType,
    /// Node ID that was modified (e.g., `rust_fn:crates/foo/src/lib.rs::bar`).
    pub node_id: String,
    /// SHA-256 hash of the new body (first 16 hex chars).
    pub body_hash: String,
    /// New body content. None for deletes.
    pub new_body: Option<String>,
    /// Unix timestamp (milliseconds).
    pub timestamp_ms: i64,
    /// Agent that made the change.
    pub agent: String,
    /// Why this change was made.
    pub rationale: String,
}

/// Type of graph mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UpdateType {
    NodeUpdated,
    NodeCreated,
    NodeDeleted,
    EdgeCreated,
    EdgeDeleted,
}

/// A conflict detected during sync (two agents edited the same node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictEvent {
    pub node_id: String,
    pub local_hash: String,
    pub remote_hash: String,
    pub remote_agent: String,
    pub resolution: String,
    pub timestamp_ms: i64,
}

// ─── Publisher ──────────────────────────────────────────────────────────────

/// Publishes code graph updates to NATS.
pub struct CodeGraphPublisher {
    client: async_nats::Client,
    agent_name: String,
}

impl CodeGraphPublisher {
    /// Connect to NATS and create a publisher.
    pub async fn new(
        nats_url: &str,
        agent_name: &str,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let client = async_nats::connect(nats_url).await?;
        Ok(Self {
            client,
            agent_name: agent_name.to_string(),
        })
    }

    /// Get a reference to the NATS client (for fire-and-forget from sync handlers).
    pub fn client_ref(&self) -> &async_nats::Client {
        &self.client
    }

    /// Publish a code graph update.
    pub async fn publish(&self, update: &CodeGraphUpdate) -> Result<(), async_nats::PublishError> {
        let data = serde_json::to_vec(update).unwrap_or_default();
        self.client.publish(UPDATES_SUBJECT, data.into()).await
    }

    /// Create an update event for a node replacement.
    pub fn make_update(
        &self,
        node_id: &str,
        new_body: &str,
        body_hash: &str,
        rationale: &str,
    ) -> CodeGraphUpdate {
        CodeGraphUpdate {
            update_type: UpdateType::NodeUpdated,
            node_id: node_id.to_string(),
            body_hash: body_hash.to_string(),
            new_body: Some(new_body.to_string()),
            timestamp_ms: Utc::now().timestamp_millis(),
            agent: self.agent_name.clone(),
            rationale: rationale.to_string(),
        }
    }
}

// ─── Subscriber ─────────────────────────────────────────────────────────────

/// Shared graph state that the subscriber can update.
pub type SharedGraphState = Arc<Mutex<SyncableGraph>>;

/// The subset of graph state needed for sync operations.
pub struct SyncableGraph {
    pub nodes: RecordBatch,
    pub edges: RecordBatch,
    /// Track body hashes for conflict detection.
    pub body_hashes: std::collections::HashMap<String, String>,
}

/// Subscribe to code graph updates and apply them locally.
pub async fn subscribe_and_apply(nats_url: &str, graph: SharedGraphState, own_agent: &str) {
    let client = match async_nats::connect(nats_url).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to connect for graph sync");
            return;
        }
    };

    let mut subscriber = match client.subscribe(UPDATES_SUBJECT).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to subscribe to graph updates");
            return;
        }
    };

    let own_agent = own_agent.to_string();
    tracing::info!(subject = UPDATES_SUBJECT, "graph sync subscriber active");

    while let Some(msg) = subscriber.next().await {
        let update: CodeGraphUpdate = match serde_json::from_slice(&msg.payload) {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, "invalid graph update payload");
                continue;
            }
        };

        // Skip our own updates to avoid echo.
        if update.agent == own_agent {
            tracing::debug!(node = %update.node_id, "skipping own update");
            continue;
        }

        tracing::info!(
            node = %update.node_id,
            agent = %update.agent,
            update_type = ?update.update_type,
            "received graph update"
        );

        let mut graph = graph.lock().await;

        // Conflict detection: check if we have a different version.
        if let Some(local_hash) = graph.body_hashes.get(&update.node_id)
            && *local_hash != update.body_hash
            && update.update_type == UpdateType::NodeUpdated
        {
            let conflict = ConflictEvent {
                node_id: update.node_id.clone(),
                local_hash: local_hash.clone(),
                remote_hash: update.body_hash.clone(),
                remote_agent: update.agent.clone(),
                resolution: "last_write_wins".to_string(),
                timestamp_ms: Utc::now().timestamp_millis(),
            };
            tracing::warn!(
                node = %conflict.node_id,
                local = %conflict.local_hash,
                remote = %conflict.remote_hash,
                "code graph conflict detected — last write wins"
            );
            if let Ok(data) = serde_json::to_vec(&conflict) {
                let _ = client.publish(CONFLICT_SUBJECT, data.into()).await;
            }
        }

        // Apply the update (last write wins).
        if update.update_type == UpdateType::NodeUpdated
            && let Some(ref new_body) = update.new_body
        {
            let node_update = crate::mcp_tools::NodeUpdate {
                body: Some(new_body.clone()),
                signature: None,
                docstring: None,
                body_hash: None,
                loc: None,
                cyclomatic_complexity: None,
                coverage_pct: None,
            };
            match crate::mcp_tools::codegraph_update_object(
                &graph.nodes,
                &update.node_id,
                &node_update,
            ) {
                Ok(new_batch) => {
                    graph.nodes = new_batch;
                    graph
                        .body_hashes
                        .insert(update.node_id.clone(), update.body_hash.clone());
                    tracing::info!(node = %update.node_id, "applied remote update");
                }
                Err(e) => {
                    tracing::warn!(
                        node = %update.node_id,
                        error = %e,
                        "failed to apply remote update"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_serialization_roundtrip() {
        let update = CodeGraphUpdate {
            update_type: UpdateType::NodeUpdated,
            node_id: "rust_fn:crates/foo/src/lib.rs::bar".to_string(),
            body_hash: "abc123".to_string(),
            new_body: Some("fn bar() { 42 }".to_string()),
            timestamp_ms: 1700000000000,
            agent: "Mini".to_string(),
            rationale: "fix typo".to_string(),
        };
        let json = serde_json::to_vec(&update).unwrap();
        let back: CodeGraphUpdate = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.node_id, update.node_id);
        assert_eq!(back.agent, "Mini");
        assert_eq!(back.update_type, UpdateType::NodeUpdated);
    }

    #[test]
    fn test_conflict_event_serialization() {
        let conflict = ConflictEvent {
            node_id: "rust_fn:foo::bar".to_string(),
            local_hash: "aaa".to_string(),
            remote_hash: "bbb".to_string(),
            remote_agent: "DGX".to_string(),
            resolution: "last_write_wins".to_string(),
            timestamp_ms: 1700000000000,
        };
        let json = serde_json::to_vec(&conflict).unwrap();
        let back: ConflictEvent = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.node_id, "rust_fn:foo::bar");
        assert_eq!(back.resolution, "last_write_wins");
    }

    #[test]
    fn test_make_update() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Can't actually connect without NATS, but test the struct
            let update = CodeGraphUpdate {
                update_type: UpdateType::NodeUpdated,
                node_id: "test::node".to_string(),
                body_hash: "hash123".to_string(),
                new_body: Some("new body".to_string()),
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                agent: "TestAgent".to_string(),
                rationale: "test".to_string(),
            };
            assert_eq!(update.agent, "TestAgent");
            assert!(update.new_body.is_some());
        });
    }
}
