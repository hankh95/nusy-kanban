//! Kanban state reader via NATS.
//!
//! Queries the kanban server using `kanban.cmd.*` request-reply,
//! parses item metadata, builds an in-memory work graph, and
//! subscribes to `kanban.event.*` for real-time state updates.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// A kanban work item as returned by the NATS server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    pub item_type: String,
    pub status: String,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub board: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub related: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub body: Option<String>,
}

/// In-memory work graph: items indexed by ID, plus dependency edges.
#[derive(Debug, Default)]
pub struct WorkGraph {
    /// All items indexed by their ID (e.g., "EX-3047").
    pub items: HashMap<String, WorkItem>,
    /// Dependency edges: key depends on values.
    pub depends_on: HashMap<String, Vec<String>>,
    /// Reverse dependency edges: key blocks values.
    pub blocks: HashMap<String, Vec<String>>,
}

impl WorkGraph {
    /// Build a work graph from a list of items.
    pub fn from_items(items: Vec<WorkItem>) -> Self {
        let mut graph = WorkGraph::default();
        for item in items {
            // Build dependency edges
            for dep in &item.depends_on {
                graph
                    .depends_on
                    .entry(item.id.clone())
                    .or_default()
                    .push(dep.clone());
                graph
                    .blocks
                    .entry(dep.clone())
                    .or_default()
                    .push(item.id.clone());
            }
            graph.items.insert(item.id.clone(), item);
        }
        graph
    }

    /// Get items filtered by status.
    pub fn items_by_status(&self, status: &str) -> Vec<&WorkItem> {
        self.items
            .values()
            .filter(|item| item.status == status)
            .collect()
    }

    /// Get items assigned to a specific agent.
    pub fn items_by_assignee(&self, assignee: &str) -> Vec<&WorkItem> {
        self.items
            .values()
            .filter(|item| item.assignee.as_deref() == Some(assignee))
            .collect()
    }

    /// Get items that are blocked (have unfinished dependencies).
    pub fn blocked_items(&self) -> Vec<&WorkItem> {
        self.items
            .values()
            .filter(|item| {
                if let Some(deps) = self.depends_on.get(&item.id) {
                    deps.iter().any(|dep_id| {
                        self.items
                            .get(dep_id)
                            .is_some_and(|dep| dep.status != "done" && dep.status != "complete")
                    })
                } else {
                    false
                }
            })
            .collect()
    }

    /// Get a summary of items by status.
    pub fn status_summary(&self) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for item in self.items.values() {
            *counts.entry(item.status.clone()).or_default() += 1;
        }
        counts
    }

    /// Apply a creation event: add a new item.
    pub fn apply_created(&mut self, item: WorkItem) {
        for dep in &item.depends_on {
            self.depends_on
                .entry(item.id.clone())
                .or_default()
                .push(dep.clone());
            self.blocks
                .entry(dep.clone())
                .or_default()
                .push(item.id.clone());
        }
        self.items.insert(item.id.clone(), item);
    }

    /// Apply a move event: update an item's status.
    pub fn apply_moved(&mut self, id: &str, new_status: &str) {
        if let Some(item) = self.items.get_mut(id) {
            item.status = new_status.to_string();
        }
    }

    /// Apply a deletion event: remove an item.
    pub fn apply_deleted(&mut self, id: &str) {
        self.items.remove(id);
        self.depends_on.remove(id);
        self.blocks.remove(id);
        // Clean up reverse edges
        for deps in self.depends_on.values_mut() {
            deps.retain(|d| d != id);
        }
        for blockers in self.blocks.values_mut() {
            blockers.retain(|b| b != id);
        }
    }
}

/// Errors from conductor NATS operations.
#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    #[error("NATS connection failed: {0}")]
    Connect(String),

    #[error("Request to kanban server failed: {0}")]
    Request(String),

    #[error("Failed to parse server response: {0}")]
    Parse(String),

    #[error("Server returned error: {error} (code: {code})")]
    ServerError { error: String, code: String },

    #[error("Request timed out after {0:?}")]
    Timeout(Duration),
}

/// Default request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Default connection timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Async NATS client for reading kanban state.
///
/// Uses noesis-ship ConnectionManager for NATS lifecycle.
/// Fully async because the conductor runs as a long-lived service.
pub struct KanbanReader {
    conn: noesis_ship::connection::ConnectionManager,
    /// Cached work graph, updated by event subscription.
    graph: RwLock<WorkGraph>,
}

impl KanbanReader {
    /// Connect to the NATS server via noesis-ship ConnectionManager.
    pub async fn connect(url: &str) -> Result<Self, ReaderError> {
        let config = noesis_ship::types::NatsConfig::new(url).with_connect_timeout(CONNECT_TIMEOUT);
        let mut conn = noesis_ship::connection::ConnectionManager::new(config);
        conn.connect()
            .await
            .map_err(|e| ReaderError::Connect(e.to_string()))?;

        Ok(KanbanReader {
            conn,
            graph: RwLock::new(WorkGraph::default()),
        })
    }

    /// Send a request to `kanban.cmd.{command}` and return parsed JSON.
    async fn request(
        &self,
        command: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, ReaderError> {
        let subject = format!("kanban.cmd.{command}");
        let body = serde_json::to_vec(payload)
            .map_err(|e| ReaderError::Request(format!("serialize: {e}")))?;

        let client = self
            .conn
            .client()
            .map_err(|e| ReaderError::Connect(e.to_string()))?;
        let response = tokio::time::timeout(REQUEST_TIMEOUT, client.request(subject, body.into()))
            .await
            .map_err(|_| ReaderError::Timeout(REQUEST_TIMEOUT))?
            .map_err(|e| ReaderError::Request(e.to_string()))?;

        let value: serde_json::Value = serde_json::from_slice(&response.payload)
            .map_err(|e| ReaderError::Parse(e.to_string()))?;

        if let Some(error) = value.get("error") {
            return Err(ReaderError::ServerError {
                error: error.as_str().unwrap_or("unknown").to_string(),
                code: value
                    .get("code")
                    .and_then(|c| c.as_str())
                    .unwrap_or("UNKNOWN")
                    .to_string(),
            });
        }

        Ok(value)
    }

    /// List all items, optionally filtered by status and/or board.
    pub async fn list_items(
        &self,
        status: Option<&str>,
        board: Option<&str>,
    ) -> Result<Vec<WorkItem>, ReaderError> {
        let mut payload = serde_json::json!({});
        if let Some(s) = status {
            payload["status"] = serde_json::Value::String(s.to_string());
        }
        if let Some(b) = board {
            payload["board"] = serde_json::Value::String(b.to_string());
        }

        let response = self.request("list", &payload).await?;
        parse_items_response(&response)
    }

    /// Show a single item by ID.
    pub async fn show_item(&self, id: &str) -> Result<WorkItem, ReaderError> {
        let payload = serde_json::json!({ "id": id });
        let response = self.request("show", &payload).await?;
        parse_single_item(&response)
    }

    /// Load all items and build the work graph. Replaces the cached graph.
    pub async fn refresh_graph(&self) -> Result<(), ReaderError> {
        let items = self.list_items(None, None).await?;
        let graph = WorkGraph::from_items(items);
        *self.graph.write().await = graph;
        Ok(())
    }

    /// Get a read reference to the cached work graph.
    pub async fn graph(&self) -> tokio::sync::RwLockReadGuard<'_, WorkGraph> {
        self.graph.read().await
    }

    /// Subscribe to kanban events and apply them to the work graph.
    ///
    /// This spawns a background task that listens for `kanban.event.*`
    /// messages and updates the cached graph. Returns the join handle.
    pub async fn subscribe_events(&self) -> Result<async_nats::Subscriber, ReaderError> {
        let client = self
            .conn
            .client()
            .map_err(|e| ReaderError::Connect(e.to_string()))?;
        client
            .subscribe("kanban.event.>")
            .await
            .map_err(|e| ReaderError::Request(format!("subscribe: {e}")))
    }

    /// Apply an event message to the cached graph.
    pub async fn apply_event(&self, subject: &str, payload: &[u8]) {
        let event_type = subject.strip_prefix("kanban.event.").unwrap_or(subject);

        match event_type {
            "created" => {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload)
                    && let (Some(id), Some(title), Some(item_type)) = (
                        value.get("id").and_then(|v| v.as_str()),
                        value.get("title").and_then(|v| v.as_str()),
                        value.get("item_type").and_then(|v| v.as_str()),
                    )
                {
                    let item = WorkItem {
                        id: id.to_string(),
                        title: title.to_string(),
                        item_type: item_type.to_string(),
                        status: "backlog".to_string(),
                        priority: None,
                        assignee: None,
                        board: value
                            .get("board")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        tags: Vec::new(),
                        related: Vec::new(),
                        depends_on: Vec::new(),
                        body: None,
                    };
                    self.graph.write().await.apply_created(item);
                }
            }
            "moved" => {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload)
                    && let (Some(id), Some(to)) = (
                        value.get("id").and_then(|v| v.as_str()),
                        value.get("to").and_then(|v| v.as_str()),
                    )
                {
                    self.graph.write().await.apply_moved(id, to);
                }
            }
            "deleted" => {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload)
                    && let Some(id) = value.get("id").and_then(|v| v.as_str())
                {
                    self.graph.write().await.apply_deleted(id);
                }
            }
            _ => {} // Ignore unknown events (e.g., stats)
        }
    }

    /// Get the underlying NATS client (for advanced use).
    pub fn nats_client(&self) -> Result<&async_nats::Client, ReaderError> {
        self.conn
            .client()
            .map_err(|e| ReaderError::Connect(e.to_string()))
    }
}

/// Parse the list response into WorkItems.
fn parse_items_response(response: &serde_json::Value) -> Result<Vec<WorkItem>, ReaderError> {
    // The server returns items as an array under "items" key
    let items_val = response
        .get("items")
        .ok_or_else(|| ReaderError::Parse("missing 'items' field in response".to_string()))?;

    let items: Vec<WorkItem> =
        serde_json::from_value(items_val.clone()).map_err(|e| ReaderError::Parse(e.to_string()))?;

    Ok(items)
}

/// Parse a single item from the show response.
fn parse_single_item(response: &serde_json::Value) -> Result<WorkItem, ReaderError> {
    // The show command returns the item directly (or under "item" key)
    if let Some(item_val) = response.get("item") {
        serde_json::from_value(item_val.clone()).map_err(|e| ReaderError::Parse(e.to_string()))
    } else {
        // Try parsing the response itself as an item
        serde_json::from_value(response.clone()).map_err(|e| ReaderError::Parse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_items() -> Vec<WorkItem> {
        vec![
            WorkItem {
                id: "EX-3001".to_string(),
                title: "Arrow schemas".to_string(),
                item_type: "expedition".to_string(),
                status: "done".to_string(),
                priority: Some("critical".to_string()),
                assignee: Some("M5".to_string()),
                board: Some("development".to_string()),
                tags: vec!["v14".to_string(), "arrow".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            },
            WorkItem {
                id: "EX-3002".to_string(),
                title: "NATS server".to_string(),
                item_type: "expedition".to_string(),
                status: "in_progress".to_string(),
                priority: Some("high".to_string()),
                assignee: Some("DGX".to_string()),
                board: Some("development".to_string()),
                tags: vec!["v14".to_string()],
                related: vec![],
                depends_on: vec!["EX-3001".to_string()],
                body: Some("## Phase 1\n- Do stuff".to_string()),
            },
            WorkItem {
                id: "EX-3003".to_string(),
                title: "Integration tests".to_string(),
                item_type: "expedition".to_string(),
                status: "backlog".to_string(),
                priority: Some("medium".to_string()),
                assignee: None,
                board: Some("development".to_string()),
                tags: vec![],
                related: vec![],
                depends_on: vec!["EX-3002".to_string()],
                body: None,
            },
            WorkItem {
                id: "CH-3010".to_string(),
                title: "Clean up CI".to_string(),
                item_type: "chore".to_string(),
                status: "backlog".to_string(),
                priority: Some("low".to_string()),
                assignee: None,
                board: Some("development".to_string()),
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            },
            WorkItem {
                id: "EX-3004".to_string(),
                title: "GPU training".to_string(),
                item_type: "expedition".to_string(),
                status: "in_progress".to_string(),
                priority: Some("high".to_string()),
                assignee: Some("DGX".to_string()),
                board: Some("development".to_string()),
                tags: vec!["gpu".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            },
        ]
    }

    #[test]
    fn test_work_graph_from_items() {
        let items = sample_items();
        let graph = WorkGraph::from_items(items);

        assert_eq!(graph.items.len(), 5);
        assert!(graph.items.contains_key("EX-3001"));
        assert!(graph.items.contains_key("EX-3002"));
    }

    #[test]
    fn test_work_graph_dependency_edges() {
        let graph = WorkGraph::from_items(sample_items());

        // EX-3002 depends on EX-3001
        let deps = graph.depends_on.get("EX-3002").expect("should have deps");
        assert!(deps.contains(&"EX-3001".to_string()));

        // EX-3001 blocks EX-3002
        let blockers = graph.blocks.get("EX-3001").expect("should have blockers");
        assert!(blockers.contains(&"EX-3002".to_string()));
    }

    #[test]
    fn test_items_by_status() {
        let graph = WorkGraph::from_items(sample_items());

        let backlog = graph.items_by_status("backlog");
        assert_eq!(backlog.len(), 2); // EX-3003 and CH-3010

        let in_progress = graph.items_by_status("in_progress");
        assert_eq!(in_progress.len(), 2); // EX-3002 and EX-3004

        let done = graph.items_by_status("done");
        assert_eq!(done.len(), 1); // EX-3001
    }

    #[test]
    fn test_items_by_assignee() {
        let graph = WorkGraph::from_items(sample_items());

        let dgx_items = graph.items_by_assignee("DGX");
        assert_eq!(dgx_items.len(), 2); // EX-3002 and EX-3004

        let m5_items = graph.items_by_assignee("M5");
        assert_eq!(m5_items.len(), 1); // EX-3001

        let mini_items = graph.items_by_assignee("Mini");
        assert_eq!(mini_items.len(), 0);
    }

    #[test]
    fn test_blocked_items() {
        let graph = WorkGraph::from_items(sample_items());

        let blocked = graph.blocked_items();
        // EX-3003 depends on EX-3002 which is in_progress (not done)
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].id, "EX-3003");
        // EX-3002 depends on EX-3001 which IS done, so not blocked
    }

    #[test]
    fn test_status_summary() {
        let graph = WorkGraph::from_items(sample_items());
        let summary = graph.status_summary();

        assert_eq!(summary.get("backlog"), Some(&2));
        assert_eq!(summary.get("in_progress"), Some(&2));
        assert_eq!(summary.get("done"), Some(&1));
    }

    #[test]
    fn test_apply_created() {
        let mut graph = WorkGraph::from_items(sample_items());
        assert_eq!(graph.items.len(), 5);

        graph.apply_created(WorkItem {
            id: "EX-3005".to_string(),
            title: "New item".to_string(),
            item_type: "expedition".to_string(),
            status: "backlog".to_string(),
            priority: None,
            assignee: None,
            board: Some("development".to_string()),
            tags: vec![],
            related: vec![],
            depends_on: vec!["EX-3001".to_string()],
            body: None,
        });

        assert_eq!(graph.items.len(), 6);
        assert!(graph.items.contains_key("EX-3005"));
        // Check dependency edge was created
        let deps = graph.depends_on.get("EX-3005").expect("should have deps");
        assert!(deps.contains(&"EX-3001".to_string()));
    }

    #[test]
    fn test_apply_moved() {
        let mut graph = WorkGraph::from_items(sample_items());
        assert_eq!(graph.items["EX-3002"].status, "in_progress");

        graph.apply_moved("EX-3002", "review");
        assert_eq!(graph.items["EX-3002"].status, "review");
    }

    #[test]
    fn test_apply_moved_nonexistent_item_is_noop() {
        let mut graph = WorkGraph::from_items(sample_items());
        graph.apply_moved("EX-9999", "done"); // Should not panic
        assert!(!graph.items.contains_key("EX-9999"));
    }

    #[test]
    fn test_apply_deleted() {
        let mut graph = WorkGraph::from_items(sample_items());
        assert_eq!(graph.items.len(), 5);

        graph.apply_deleted("EX-3001");
        assert_eq!(graph.items.len(), 4);
        assert!(!graph.items.contains_key("EX-3001"));
        // Reverse edges cleaned up
        assert!(!graph.blocks.contains_key("EX-3001"));
    }

    #[test]
    fn test_status_summary_with_20_items() {
        // Verify the reader handles 20+ items correctly (expedition requirement)
        let items: Vec<WorkItem> = (0..25)
            .map(|i| {
                let status = match i % 4 {
                    0 => "backlog",
                    1 => "in_progress",
                    2 => "review",
                    _ => "done",
                };
                WorkItem {
                    id: format!("EX-{}", 4000 + i),
                    title: format!("Item {i}"),
                    item_type: "expedition".to_string(),
                    status: status.to_string(),
                    priority: Some("medium".to_string()),
                    assignee: if i % 3 == 0 {
                        Some("M5".to_string())
                    } else {
                        None
                    },
                    board: Some("development".to_string()),
                    tags: vec![],
                    related: vec![],
                    depends_on: vec![],
                    body: None,
                }
            })
            .collect();

        let graph = WorkGraph::from_items(items);
        let summary = graph.status_summary();

        assert_eq!(graph.items.len(), 25);
        // 25 items split across 4 statuses: 7+6+6+6
        assert_eq!(summary.values().sum::<usize>(), 25);
        assert_eq!(summary.get("backlog"), Some(&7));
        assert_eq!(summary.get("in_progress"), Some(&6));
    }

    #[test]
    fn test_parse_items_response() {
        let response = serde_json::json!({
            "items": [
                {
                    "id": "EX-3001",
                    "title": "Test",
                    "item_type": "expedition",
                    "status": "backlog"
                }
            ]
        });

        let items = parse_items_response(&response).expect("should parse");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "EX-3001");
        assert_eq!(items[0].status, "backlog");
        // Optional fields should have defaults
        assert!(items[0].assignee.is_none());
        assert!(items[0].tags.is_empty());
    }

    #[test]
    fn test_parse_items_response_missing_items_field() {
        let response = serde_json::json!({ "count": 0 });
        let result = parse_items_response(&response);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_single_item_with_item_key() {
        let response = serde_json::json!({
            "item": {
                "id": "EX-3001",
                "title": "Test",
                "item_type": "expedition",
                "status": "in_progress",
                "assignee": "M5",
                "body": "## Phase 1\n- stuff"
            }
        });

        let item = parse_single_item(&response).expect("should parse");
        assert_eq!(item.id, "EX-3001");
        assert_eq!(item.assignee, Some("M5".to_string()));
        assert!(item.body.is_some());
    }

    #[test]
    fn test_parse_single_item_direct() {
        let response = serde_json::json!({
            "id": "EX-3001",
            "title": "Test",
            "item_type": "expedition",
            "status": "done"
        });

        let item = parse_single_item(&response).expect("should parse");
        assert_eq!(item.id, "EX-3001");
        assert_eq!(item.status, "done");
    }
}
