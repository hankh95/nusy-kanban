//! Dependency graph relation handlers.

use super::{error_response, parse_payload, serialize_response};
use nusy_kanban::relations::RelationsStore;
use serde::Deserialize;

#[derive(Deserialize)]
struct RelationAddRequest {
    source_id: String,
    target_id: String,
    predicate: String,
}

pub(crate) fn handle_relation_add(
    payload: &[u8],
    relations: &mut RelationsStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: RelationAddRequest = parse_payload(payload)?;

    let rel_id = relations
        .add_relation(&req.source_id, &req.target_id, &req.predicate)
        .map_err(|e| error_response(&format!("{e}"), "RELATION_ADD_FAILED"))?;

    serialize_response(&serde_json::json!({
        "relation_id": rel_id,
        "source": req.source_id,
        "target": req.target_id,
        "predicate": req.predicate,
    }))
}

// ── Relation Query ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RelationQueryRequest {
    item_id: String,
}

pub(crate) fn handle_relation_query(
    payload: &[u8],
    relations: &RelationsStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: RelationQueryRequest = parse_payload(payload)?;
    let results = relations.query_relations(&req.item_id);

    serialize_response(&serde_json::json!({
        "item_id": req.item_id,
        "count": results.iter().map(|b| b.num_rows()).sum::<usize>(),
    }))
}
