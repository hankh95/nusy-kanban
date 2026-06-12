//! Event broadcasting for mutations.
//!
//! After every mutation (create, move, delete), the server publishes an event
//! to `kanban.event.{type}`. Consumers like Command Deck can subscribe for
//! real-time updates.

use serde::Serialize;

/// Event published after an item is created.
#[derive(Debug, Serialize)]
pub struct ItemCreated {
    pub id: String,
    pub title: String,
    pub item_type: String,
    pub board: String,
    pub agent: Option<String>,
}

/// Event published after an item is moved to a new status.
#[derive(Debug, Serialize)]
pub struct ItemMoved {
    pub id: String,
    pub from: String,
    pub to: String,
    pub agent: Option<String>,
}

/// Event published after an item is deleted.
#[derive(Debug, Serialize)]
pub struct ItemDeleted {
    pub id: String,
}

/// Periodic stats broadcast.
#[derive(Debug, Serialize)]
pub struct StatsSnapshot {
    pub total_items: usize,
    pub active_items: usize,
    pub by_status: Vec<(String, usize)>,
    pub timestamp: String,
}

/// Subject names for events.
pub mod subjects {
    pub const CREATED: &str = "kanban.event.created";
    pub const MOVED: &str = "kanban.event.moved";
    pub const DELETED: &str = "kanban.event.deleted";
    pub const STATS: &str = "kanban.event.stats";
}

/// Serialize an event to JSON bytes for NATS publishing.
pub fn to_event_bytes<T: Serialize>(event: &T) -> Vec<u8> {
    serde_json::to_vec(event).unwrap_or_else(|_| b"{}".to_vec())
}

/// Detect mutation events from a dispatch response.
///
/// Given a command name and the JSON response bytes, returns
/// `Some((event_type_suffix, event_bytes))` if the command was a mutation
/// that succeeded. Used by NatsServiceBuilder's mutation callback.
pub fn detect_mutation(command: &str, response: &[u8]) -> Option<(String, Vec<u8>)> {
    let resp: serde_json::Value = serde_json::from_slice(response).ok()?;
    if resp.get("error").is_some() {
        return None;
    }

    match command {
        "create" | "hdd.paper" | "hdd.hypothesis" | "hdd.experiment" | "hdd.measure"
        | "hdd.idea" | "hdd.literature" => {
            let item_type_str = resp
                .get("item_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let board = nusy_kanban::ItemType::from_str_loose(item_type_str)
                .map(|t| t.board().to_string())
                .unwrap_or_else(|| "development".to_string());
            Some((
                "created".to_string(),
                to_event_bytes(&ItemCreated {
                    id: resp
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    title: resp
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    item_type: item_type_str.to_string(),
                    board,
                    agent: None,
                }),
            ))
        }
        "move" => Some((
            "moved".to_string(),
            to_event_bytes(&ItemMoved {
                id: resp
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                from: resp
                    .get("from")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                to: resp
                    .get("to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                agent: None,
            }),
        )),
        "delete" => Some((
            "deleted".to_string(),
            to_event_bytes(&ItemDeleted {
                id: resp
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_item_created_serialization() {
        let event = ItemCreated {
            id: "EXP-100".to_string(),
            title: "Test".to_string(),
            item_type: "expedition".to_string(),
            board: "development".to_string(),
            agent: Some("M5".to_string()),
        };
        let bytes = to_event_bytes(&event);
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["id"], "EXP-100");
        assert_eq!(parsed["agent"], "M5");
    }

    #[test]
    fn test_item_moved_serialization() {
        let event = ItemMoved {
            id: "EXP-100".to_string(),
            from: "backlog".to_string(),
            to: "in_progress".to_string(),
            agent: None,
        };
        let bytes = to_event_bytes(&event);
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["from"], "backlog");
        assert_eq!(parsed["to"], "in_progress");
        assert!(parsed["agent"].is_null());
    }

    #[test]
    fn test_stats_snapshot_serialization() {
        let event = StatsSnapshot {
            total_items: 42,
            active_items: 38,
            by_status: vec![("backlog".to_string(), 20), ("in_progress".to_string(), 10)],
            timestamp: "2026-03-14T20:00:00Z".to_string(),
        };
        let bytes = to_event_bytes(&event);
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["total_items"], 42);
        assert_eq!(parsed["active_items"], 38);
    }

    // ── Mutation detection tests (Phase 4 — event emission via callback) ────

    #[test]
    fn test_detect_mutation_create_emits_created_event() {
        let response = serde_json::to_vec(&serde_json::json!({
            "id": "EX-3001",
            "title": "Test expedition",
            "item_type": "expedition",
        }))
        .unwrap();

        let result = detect_mutation("create", &response);
        assert!(result.is_some(), "create should emit an event");

        let (event_type, event_bytes) = result.unwrap();
        assert_eq!(event_type, "created");

        let parsed: serde_json::Value = serde_json::from_slice(&event_bytes).unwrap();
        assert_eq!(parsed["id"], "EX-3001");
        assert_eq!(parsed["title"], "Test expedition");
        assert_eq!(parsed["item_type"], "expedition");
        assert_eq!(parsed["board"], "development");
    }

    #[test]
    fn test_detect_mutation_move_emits_moved_event() {
        let response = serde_json::to_vec(&serde_json::json!({
            "id": "EX-3001",
            "from": "backlog",
            "to": "in_progress",
        }))
        .unwrap();

        let result = detect_mutation("move", &response);
        assert!(result.is_some(), "move should emit an event");

        let (event_type, event_bytes) = result.unwrap();
        assert_eq!(event_type, "moved");

        let parsed: serde_json::Value = serde_json::from_slice(&event_bytes).unwrap();
        assert_eq!(parsed["id"], "EX-3001");
        assert_eq!(parsed["from"], "backlog");
        assert_eq!(parsed["to"], "in_progress");
    }

    #[test]
    fn test_detect_mutation_delete_emits_deleted_event() {
        let response = serde_json::to_vec(&serde_json::json!({
            "id": "EX-3001",
        }))
        .unwrap();

        let result = detect_mutation("delete", &response);
        assert!(result.is_some());

        let (event_type, event_bytes) = result.unwrap();
        assert_eq!(event_type, "deleted");

        let parsed: serde_json::Value = serde_json::from_slice(&event_bytes).unwrap();
        assert_eq!(parsed["id"], "EX-3001");
    }

    #[test]
    fn test_detect_mutation_error_response_returns_none() {
        let response = serde_json::to_vec(&serde_json::json!({
            "error": "item not found",
            "code": "NOT_FOUND",
        }))
        .unwrap();

        assert!(detect_mutation("create", &response).is_none());
        assert!(detect_mutation("move", &response).is_none());
        assert!(detect_mutation("delete", &response).is_none());
    }

    #[test]
    fn test_detect_mutation_read_commands_return_none() {
        let response = serde_json::to_vec(&serde_json::json!({
            "items": [],
        }))
        .unwrap();

        assert!(detect_mutation("list", &response).is_none());
        assert!(detect_mutation("show", &response).is_none());
        assert!(detect_mutation("board", &response).is_none());
        assert!(detect_mutation("stats", &response).is_none());
        assert!(detect_mutation("query", &response).is_none());
    }
}
