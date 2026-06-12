//! Core CRUD and analytics handlers.

use super::{error_response, parse_payload, serialize_response, states_as_strings};
use nusy_kanban::crud::{CreateItemInput, KanbanStore};
use nusy_kanban::item_type::ItemType;
use nusy_kanban::relations::RelationsStore;
use nusy_kanban::{critical_path, display, export, query};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct CreateRequest {
    title: String,
    item_type: String,
    priority: Option<String>,
    assignee: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    related: Vec<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    body: Option<String>,
}

#[derive(Serialize)]
struct CreateResponse {
    id: String,
    title: String,
    item_type: String,
    status: String,
}

pub(crate) fn handle_create(payload: &[u8], store: &mut KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: CreateRequest = parse_payload(payload)?;

    let item_type = ItemType::from_str_loose(&req.item_type).ok_or_else(|| {
        error_response(
            &format!("Unknown item type: {}", req.item_type),
            "INVALID_TYPE",
        )
    })?;

    let input = CreateItemInput {
        title: req.title.clone(),
        item_type,
        priority: req.priority,
        assignee: req.assignee,
        tags: req.tags,
        related: req.related,
        depends_on: req.depends_on,
        body: req.body,
    };

    let id = store
        .create_item(&input)
        .map_err(|e| error_response(&format!("{e}"), "CREATE_FAILED"))?;

    serialize_response(&CreateResponse {
        id,
        title: req.title,
        item_type: req.item_type,
        status: "backlog".to_string(),
    })
}

// ── Move ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MoveRequest {
    id: String,
    status: String,
    assignee: Option<String>,
    #[serde(default)]
    force: bool,
    reason: Option<String>,
    resolution: Option<String>,
    closed_by: Option<String>,
}

#[derive(Serialize)]
struct MoveResponse {
    id: String,
    from: String,
    to: String,
    resolution: Option<String>,
}

pub(crate) fn handle_move(payload: &[u8], store: &mut KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: MoveRequest = parse_payload(payload)?;

    // Validate resolution before the move
    nusy_kanban::state_machine::validate_resolution(req.resolution.as_deref(), &req.status)
        .map_err(|e| error_response(&format!("{e}"), "INVALID_RESOLUTION"))?;

    // Get current status before the move
    let item = store
        .get_item(&req.id)
        .map_err(|e| error_response(&format!("{e}"), "NOT_FOUND"))?;

    let from_status = {
        use arrow::array::Array;
        let col = item
            .column(nusy_kanban::schema::items_col::STATUS)
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .expect("status column");
        col.value(0).to_string()
    };

    store
        .update_status(
            &req.id,
            &req.status,
            req.assignee.as_deref(),
            req.force,
            req.reason.as_deref(),
        )
        .map_err(|e| error_response(&format!("{e}"), "MOVE_FAILED"))?;

    // Apply resolution if provided
    if let Some(ref res) = req.resolution {
        store
            .update_resolution(&req.id, Some(res))
            .map_err(|e| error_response(&format!("{e}"), "RESOLUTION_FAILED"))?;
    }

    // Apply closed_by if provided
    if let Some(ref cb) = req.closed_by {
        store
            .update_closed_by(&req.id, Some(cb))
            .map_err(|e| error_response(&format!("{e}"), "CLOSED_BY_FAILED"))?;
    }

    serialize_response(&MoveResponse {
        id: req.id,
        from: from_status,
        to: req.status,
        resolution: req.resolution,
    })
}

// ── Update ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct UpdateRequest {
    id: String,
    title: Option<String>,
    priority: Option<String>,
    assignee: Option<String>,
    tags: Option<Vec<String>>,
    body: Option<String>,
    related: Option<Vec<String>>,
    depends_on: Option<Vec<String>>,
}

pub(crate) fn handle_update(payload: &[u8], store: &mut KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: UpdateRequest = parse_payload(payload)?;

    // Verify item exists
    store
        .get_item(&req.id)
        .map_err(|e| error_response(&format!("{e}"), "NOT_FOUND"))?;

    let mut updated = Vec::new();

    if let Some(ref title) = req.title {
        store
            .update_title(&req.id, title)
            .map_err(|e| error_response(&format!("{e}"), "UPDATE_FAILED"))?;
        updated.push("title");
    }
    if let Some(ref priority) = req.priority {
        store
            .update_priority(&req.id, Some(priority))
            .map_err(|e| error_response(&format!("{e}"), "UPDATE_FAILED"))?;
        updated.push("priority");
    }
    if let Some(ref assignee) = req.assignee {
        store
            .update_assignee(&req.id, Some(assignee))
            .map_err(|e| error_response(&format!("{e}"), "UPDATE_FAILED"))?;
        updated.push("assignee");
    }
    if let Some(ref tags) = req.tags {
        store
            .update_tags(&req.id, tags)
            .map_err(|e| error_response(&format!("{e}"), "UPDATE_FAILED"))?;
        updated.push("tags");
    }
    if let Some(ref body) = req.body {
        store
            .update_body(&req.id, Some(body))
            .map_err(|e| error_response(&format!("{e}"), "UPDATE_FAILED"))?;
        updated.push("body");
    }
    if let Some(ref related) = req.related {
        store
            .update_related(&req.id, related)
            .map_err(|e| error_response(&format!("{e}"), "UPDATE_FAILED"))?;
        updated.push("related");
    }
    if let Some(ref depends_on) = req.depends_on {
        store
            .update_depends_on(&req.id, depends_on)
            .map_err(|e| error_response(&format!("{e}"), "UPDATE_FAILED"))?;
        updated.push("depends_on");
    }

    serialize_response(&serde_json::json!({
        "id": req.id,
        "updated": updated,
    }))
}

// ── Comment ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CommentRequest {
    id: String,
    text: String,
    agent: Option<String>,
}

pub(crate) fn handle_comment(payload: &[u8], store: &mut KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: CommentRequest = parse_payload(payload)?;

    // Verify item exists
    store
        .get_item(&req.id)
        .map_err(|e| error_response(&format!("{e}"), "NOT_FOUND"))?;

    store
        .add_comment(&req.id, &req.text, req.agent.as_deref())
        .map_err(|e| error_response(&format!("{e}"), "COMMENT_FAILED"))?;

    serialize_response(&serde_json::json!({
        "id": req.id,
        "comment": req.text,
    }))
}

// ── Rank ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RankRequest {
    id: String,
    /// Manual rank. `Some(n)` sets it (lower = higher priority); `None` clears it.
    rank: Option<i32>,
}

pub(crate) fn handle_rank(payload: &[u8], store: &mut KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: RankRequest = parse_payload(payload)?;

    // Verify item exists (so a typo'd ID errors instead of silently no-op'ing).
    store
        .get_item(&req.id)
        .map_err(|e| error_response(&format!("{e}"), "NOT_FOUND"))?;

    store
        .update_rank(&req.id, req.rank)
        .map_err(|e| error_response(&format!("{e}"), "RANK_FAILED"))?;

    serialize_response(&serde_json::json!({
        "id": req.id,
        "rank": req.rank,
    }))
}

// ── List ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ListRequest {
    status: Option<String>,
    item_type: Option<String>,
    board: Option<String>,
    assignee: Option<String>,
    /// CH-4307: post-filter by resolution (terminal states only — completed,
    /// superseded, wont_do, duplicate, obsolete, merged).
    #[serde(default)]
    resolution: Option<String>,
    /// CH-4307: post-filter by priority (critical, high, medium, low).
    #[serde(default)]
    priority: Option<String>,
    /// CH-4307: post-filter by tag (exact match, multiple = AND). The client
    /// sends `Vec<String>` under the `tags` key; default to empty so older
    /// clients without the field continue to work.
    #[serde(default)]
    tags: Vec<String>,
    /// CH-4307: post-filter to items with all dependencies met (unblocked).
    #[serde(default)]
    ready: bool,
}

pub(crate) fn handle_list(payload: &[u8], store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: ListRequest = parse_payload(payload)?;

    let mut items = store.query_items(
        req.status.as_deref(),
        req.item_type.as_deref(),
        req.board.as_deref(),
        req.assignee.as_deref(),
    );

    // CH-4307: apply the post-filters that previously only existed in the
    // local-mode handler (`Commands::List` in nusy-kanban/src/main.rs:1992).
    // Server-mode requests were silently dropping these fields because they
    // were not on `ListRequest` at all, so `nk list --tag X` returned the full
    // board regardless of tag.
    if let Some(ref res_filter) = req.resolution {
        items.retain(|batch| {
            let res_col = batch
                .column(nusy_kanban::schema::items_col::RESOLUTION)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("resolution column");
            !arrow::array::Array::is_null(res_col, 0) && res_col.value(0) == res_filter.as_str()
        });
    }

    if let Some(ref pri_filter) = req.priority {
        items.retain(|batch| {
            let pri_col = batch
                .column(nusy_kanban::schema::items_col::PRIORITY)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("priority column");
            !arrow::array::Array::is_null(pri_col, 0) && pri_col.value(0) == pri_filter.as_str()
        });
    }

    if !req.tags.is_empty() {
        let needles: Vec<&str> = req.tags.iter().map(String::as_str).collect();
        items.retain(|batch| {
            let tags_col = batch
                .column(nusy_kanban::schema::items_col::TAGS)
                .as_any()
                .downcast_ref::<arrow::array::ListArray>()
                .expect("tags column");
            // query_items returns one-row-per-batch slices, but be defensive
            // against future batching by checking every row.
            (0..batch.num_rows()).any(|i| {
                if arrow::array::Array::is_null(tags_col, i) {
                    return false;
                }
                let item_tags = tags_col.value(i);
                let tag_arr = item_tags
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .expect("tag values");
                let len = arrow::array::Array::len(tag_arr);
                let item_tag_set: std::collections::HashSet<&str> =
                    (0..len).map(|j| tag_arr.value(j)).collect();
                needles.iter().all(|t| item_tag_set.contains(*t))
            })
        });
    }

    if req.ready {
        // Recompute against the unfiltered store so dependency resolution sees
        // the full graph, then narrow to items whose ID is in the ready set.
        let all_batches = store.query_items(None, None, None, None);
        let extracted = critical_path::extract_items(&all_batches);
        match critical_path::compute_critical_path(&extracted) {
            Ok(cp) => {
                let ready_set: std::collections::HashSet<&str> =
                    cp.ready.iter().map(String::as_str).collect();
                items.retain(|batch| {
                    let ids = batch
                        .column(nusy_kanban::schema::items_col::ID)
                        .as_any()
                        .downcast_ref::<arrow::array::StringArray>()
                        .expect("id");
                    (0..batch.num_rows()).any(|i| ready_set.contains(ids.value(i)))
                });
            }
            Err(e) => {
                return Err(error_response(
                    &format!("ready filter failed: {e}"),
                    "READY_FILTER_FAILED",
                ));
            }
        }
    }

    let table = display::format_item_table(&items);
    serialize_response(&serde_json::json!({
        "count": items.iter().map(|b| b.num_rows()).sum::<usize>(),
        "table": table,
    }))
}

// ── Show ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ShowRequest {
    id: String,
    format: Option<String>,
}

pub(crate) fn handle_show(payload: &[u8], store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: ShowRequest = parse_payload(payload)?;

    let item = store
        .get_item(&req.id)
        .map_err(|e| error_response(&format!("{e}"), "NOT_FOUND"))?;

    match req.format.as_deref() {
        Some("md") => {
            let md = export::item_to_markdown(&item, 0);
            serialize_response(&serde_json::json!({
                "id": req.id,
                "markdown": md,
            }))
        }
        Some("json") => {
            let json_str = export::export_json(&[item]);
            serialize_response(&serde_json::json!({
                "id": req.id,
                "json": json_str,
            }))
        }
        _ => {
            let mut detail = display::format_item_detail(&item);
            let item_comments = store.list_comments(&req.id);
            if !item_comments.is_empty() {
                detail.push_str(&nusy_kanban::comments::format_comments(&item_comments));
            }
            serialize_response(&serde_json::json!({
                "id": req.id,
                "detail": detail,
            }))
        }
    }
}

// ── Query ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct QueryRequest {
    query: String,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    top: Option<usize>,
    #[serde(default)]
    #[allow(dead_code)] // Reserved for future semantic search via Ollama
    embedding_provider: Option<String>,
}

pub(crate) fn handle_query(
    payload: &[u8],
    store: &KanbanStore,
    proposals: &nusy_graph_review::ProposalStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: QueryRequest = parse_payload(payload)?;

    // If --search is provided, do text/semantic search with top-N limiting
    if let Some(ref search_text) = req.search {
        let top = req.top.unwrap_or(10);
        let all = store.query_items(None, None, None, None);

        // Text-based search: filter items whose title contains the search term
        let search_lower = search_text.to_lowercase();
        let mut matched: Vec<arrow::array::RecordBatch> = all
            .into_iter()
            .filter(|batch| {
                let titles = batch
                    .column(nusy_kanban::schema::items_col::TITLE)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .expect("title");
                let ids = batch
                    .column(nusy_kanban::schema::items_col::ID)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .expect("id");
                let bodies = batch
                    .column(nusy_kanban::schema::items_col::BODY)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .expect("body");
                let tags = batch
                    .column(nusy_kanban::schema::items_col::TAGS)
                    .as_any()
                    .downcast_ref::<arrow::array::ListArray>()
                    .expect("tags");
                (0..batch.num_rows()).any(|i| {
                    titles.value(i).to_lowercase().contains(&search_lower)
                        || ids.value(i).to_lowercase().contains(&search_lower)
                        || (!arrow::array::Array::is_null(bodies, i)
                            && bodies.value(i).to_lowercase().contains(&search_lower))
                        || tag_contains(tags, i, &search_lower)
                })
            })
            .collect();

        // Apply top-N limit
        matched.truncate(top);

        // Federated proposal search
        let prop_batches = proposals.search_proposals(search_text).unwrap_or_default();
        let proposal_table = format_proposal_search_results(&prop_batches);

        let mut table = display::format_item_table(&matched);
        if !proposal_table.is_empty() {
            table.push('\n');
            table.push_str(&proposal_table);
        }

        return serialize_response(&serde_json::json!({
            "query": search_text,
            "count": matched.len() + prop_batches.iter().map(|b| b.num_rows()).sum::<usize>(),
            "table": table,
        }));
    }

    // Standard NL query path
    let filters = query::parse_nl_query(&req.query);
    let items = store.query_items(
        filters.status.as_deref(),
        filters.item_type.as_deref(),
        filters.board.as_deref(),
        filters.assignee.as_deref(),
    );

    // Federated proposal search on NL queries too
    let prop_batches = proposals.search_proposals(&req.query).unwrap_or_default();
    let proposal_table = format_proposal_search_results(&prop_batches);

    let mut table = display::format_item_table(&items);
    if !proposal_table.is_empty() {
        table.push('\n');
        table.push_str(&proposal_table);
    }

    serialize_response(&serde_json::json!({
        "query": req.query,
        "count": items.iter().map(|b| b.num_rows()).sum::<usize>() + prop_batches.iter().map(|b| b.num_rows()).sum::<usize>(),
        "table": table,
    }))
}

/// Format proposal RecordBatches as a display table string for query results.
fn format_proposal_search_results(batches: &[arrow::record_batch::RecordBatch]) -> String {
    use arrow::array::Array;
    use nusy_graph_review::schema::proposals_col;

    if batches.is_empty() {
        return String::new();
    }

    let mut lines = vec!["[Proposals]".to_string()];
    for batch in batches {
        let ids = batch
            .column(proposals_col::PROPOSAL_ID)
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .expect("id");
        let titles = batch
            .column(proposals_col::TITLE)
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .expect("title");
        let statuses = batch
            .column(proposals_col::STATUS)
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .expect("status");
        let authors = batch
            .column(proposals_col::AUTHOR)
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .expect("author");
        for i in 0..batch.num_rows() {
            lines.push(format!(
                "  {}  {}  {}  {}",
                ids.value(i),
                titles.value(i),
                statuses.value(i),
                authors.value(i),
            ));
        }
    }
    lines.join("\n")
}

/// Check if a ListArray tag column contains a search term at row `i`.
pub(crate) fn tag_contains(tags: &arrow::array::ListArray, row: usize, search: &str) -> bool {
    use arrow::array::Array;
    if tags.is_null(row) || tags.value(row).is_empty() {
        return false;
    }
    let values = tags.value(row);
    let str_arr = values
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .expect("tag string values");
    (0..str_arr.len())
        .any(|j| !str_arr.is_null(j) && str_arr.value(j).to_lowercase().contains(search))
}

// ── Board ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct BoardRequest {
    board: Option<String>,
}

pub(crate) fn handle_board(payload: &[u8], store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: BoardRequest = parse_payload(payload)?;
    let board_name = req.board.as_deref().unwrap_or("development");

    let items = store.query_items(None, None, Some(board_name), None);
    let states = states_as_strings();
    let view = display::format_board_view(&items, &states);
    serialize_response(&serde_json::json!({
        "board": board_name,
        "view": view,
    }))
}

// ── Stats ───────────────────────────────────────────────────────────────────

pub(crate) fn handle_stats(payload: &[u8], store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    // Parse optional flags from payload
    let req: serde_json::Value = serde_json::from_slice(payload).unwrap_or(serde_json::json!({}));

    let velocity = req
        .get("velocity")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let burndown = req
        .get("burndown")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let by_agent = req
        .get("by_agent")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let weeks = req.get("weeks").and_then(|v| v.as_u64()).unwrap_or(4) as u32;
    let since = req.get("since").and_then(|v| v.as_str());

    // Dispatch to the appropriate stats function
    if velocity {
        let data = nusy_kanban::stats::compute_velocity(store.runs_batches(), weeks);
        let formatted = nusy_kanban::stats::format_velocity(&data);
        return serialize_response(&serde_json::json!({ "stats": formatted }));
    }

    if burndown {
        let since_ms = since
            .and_then(nusy_kanban::stats::parse_date_to_ms)
            .unwrap_or_else(|| {
                chrono::Utc::now().timestamp_millis() - (weeks as i64 * 7 * 24 * 60 * 60 * 1000)
            });
        let data = nusy_kanban::stats::compute_burndown(
            store.items_batches(),
            store.runs_batches(),
            since_ms,
        );
        let formatted = nusy_kanban::stats::format_burndown(&data);
        return serialize_response(&serde_json::json!({ "stats": formatted }));
    }

    if by_agent {
        let data = nusy_kanban::stats::compute_agent_stats(store.runs_batches());
        let formatted = nusy_kanban::stats::format_agent_stats(&data);
        return serialize_response(&serde_json::json!({ "stats": formatted }));
    }

    // Default: basic stats
    let states = states_as_strings();
    let stats = display::format_stats(store.items_batches(), &states);
    serialize_response(&serde_json::json!({
        "stats": stats,
    }))
}

// ── Delete ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DeleteRequest {
    id: String,
}

#[derive(Serialize)]
struct DeleteResponse {
    id: String,
    deleted: bool,
}

pub(crate) fn handle_delete(payload: &[u8], store: &mut KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: DeleteRequest = parse_payload(payload)?;

    store
        .delete_item(&req.id)
        .map_err(|e| error_response(&format!("{e}"), "DELETE_FAILED"))?;

    serialize_response(&DeleteResponse {
        id: req.id,
        deleted: true,
    })
}

// ── Validate (HDD) ─────────────────────────────────────────────────────────

pub(crate) fn handle_validate(
    store: &KanbanStore,
    relations: &RelationsStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let issues = nusy_kanban::validate_hdd(store, relations);
    serialize_response(&serde_json::json!({
        "valid": issues.is_empty(),
        "issue_count": issues.len(),
        "issues": issues,
    }))
}

// ── Export ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ExportRequest {
    id: String,
}

pub(crate) fn handle_export(payload: &[u8], store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: ExportRequest = parse_payload(payload)?;

    let item = store
        .get_item(&req.id)
        .map_err(|e| error_response(&format!("{e}"), "NOT_FOUND"))?;

    let markdown = export::item_to_markdown(&item, 0);
    serialize_response(&serde_json::json!({
        "id": req.id,
        "format": "markdown",
        "content": markdown,
    }))
}

// ── Roadmap / Critical Path / Worklist ───────────────────────────────────────
//
// Graph computation on the server's RecordBatches — no data leaves the server.

#[derive(Deserialize)]
struct RoadmapRequest {
    #[serde(default)]
    flat: bool,
    #[serde(default)]
    ready: bool,
}

pub(crate) fn handle_roadmap(payload: &[u8], store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: RoadmapRequest = parse_payload(payload)?;
    let all_batches = store.query_items(None, None, None, None);

    if all_batches.is_empty() {
        return serialize_response(&serde_json::json!({ "view": "No items found.\n" }));
    }

    let items = critical_path::extract_items(&all_batches);
    let cp = critical_path::compute_critical_path(&items)
        .map_err(|e| error_response(&e, "CYCLE_DETECTED"))?;

    let view = if req.flat {
        let mut backlog: Vec<_> = items.iter().filter(|i| i.status == "backlog").collect();
        backlog.sort_by_key(|i| critical_path::sort_key(i));
        let mut lines = vec![format!(
            "Roadmap (flat, ranked by Captain rank then priority — {} backlog items):\n",
            backlog.len()
        )];
        lines.push(format!(
            "  {:<14}{:<50}{:<10}{:<10}",
            "ID", "TITLE", "PRIORITY", "ASSIGNEE"
        ));
        for item in &backlog {
            let title = critical_path::truncate(&item.title, 48);
            lines.push(format!(
                "  {:<14}{:<50}{:<10}{:<10}",
                item.id, title, item.priority, item.assignee
            ));
        }
        lines.join("\n")
    } else if req.ready {
        let ready_items: Vec<_> = items.iter().filter(|i| cp.ready.contains(&i.id)).collect();
        let mut lines = vec![format!("Ready Items ({}):\n", ready_items.len())];
        for item in &ready_items {
            lines.push(format!(
                "  {} {} [{}] ({})",
                item.id, item.title, item.status, item.priority
            ));
        }
        if ready_items.is_empty() {
            lines.push("  (none)".into());
        }
        lines.join("\n")
    } else {
        let (groups, orphans) = critical_path::group_by_voyage(&items);
        critical_path::format_roadmap(&items, &groups, &orphans, &cp)
    };

    serialize_response(&serde_json::json!({ "view": view }))
}

pub(crate) fn handle_critical_path(store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let all_batches = store.query_items(None, None, None, None);

    if all_batches.is_empty() {
        return serialize_response(&serde_json::json!({ "view": "No items found.\n" }));
    }

    let items = critical_path::extract_items(&all_batches);
    let cp = critical_path::compute_critical_path(&items)
        .map_err(|e| error_response(&e, "CYCLE_DETECTED"))?;
    let view = critical_path::format_critical_path(&items, &cp);

    serialize_response(&serde_json::json!({ "view": view }))
}

#[derive(Deserialize)]
struct WorklistRequest {
    #[serde(default = "default_agents")]
    agents: String,
    #[serde(default = "default_depth")]
    depth: usize,
}

pub(crate) fn default_agents() -> String {
    "DGX,M5,Mini".into()
}

pub(crate) fn default_depth() -> usize {
    3
}

pub(crate) fn handle_worklist(payload: &[u8], store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: WorklistRequest = parse_payload(payload)?;
    let all_batches = store.query_items(None, None, None, None);

    if all_batches.is_empty() {
        return serialize_response(&serde_json::json!({ "view": "No items found.\n" }));
    }

    let items = critical_path::extract_items(&all_batches);
    let cp = critical_path::compute_critical_path(&items)
        .map_err(|e| error_response(&e, "CYCLE_DETECTED"))?;
    let agent_list: Vec<String> = req
        .agents
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let worklist = critical_path::generate_worklist(&items, &cp, &agent_list, req.depth);
    let view = critical_path::format_worklist(&worklist);

    serialize_response(&serde_json::json!({ "view": view }))
}

// ── Next ID ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct NextIdRequest {
    item_type: String,
}

pub(crate) fn handle_next_id(payload: &[u8], store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: NextIdRequest = parse_payload(payload)?;

    let item_type = ItemType::from_str_loose(&req.item_type).ok_or_else(|| {
        error_response(
            &format!("Unknown item type: {}", req.item_type),
            "INVALID_TYPE",
        )
    })?;

    let next = nusy_kanban::allocate_id(store.items_batches(), item_type);
    serialize_response(&serde_json::json!({
        "next_id": next,
    }))
}

// ── History ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct HistoryRequest {
    #[serde(default)]
    week: bool,
    #[serde(default)]
    month: bool,
    since: Option<String>,
    by_assignee: Option<String>,
}

pub(crate) fn handle_history(payload: &[u8], store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    let req: HistoryRequest = parse_payload(payload)?;

    // Use the new filter_history when any enhanced flag is set
    if req.month || req.since.is_some() || req.by_assignee.is_some() {
        let since_ms = if let Some(ref date) = req.since {
            nusy_kanban::stats::parse_date_to_ms(date)
                .ok_or_else(|| error_response("Invalid date format (use YYYY-MM-DD)", "BAD_DATE"))?
        } else if req.month {
            chrono::Utc::now().timestamp_millis() - (30 * 24 * 60 * 60 * 1000)
        } else {
            chrono::Utc::now().timestamp_millis() - (7 * 24 * 60 * 60 * 1000)
        };

        let entries = nusy_kanban::stats::filter_history(
            store.items_batches(),
            store.runs_batches(),
            since_ms,
            req.by_assignee.as_deref(),
        );
        let formatted = nusy_kanban::stats::format_history_entries(&entries);
        return serialize_response(&serde_json::json!({ "history": formatted }));
    }

    if req.week {
        let cutoff = chrono::Utc::now().timestamp_millis() - (7 * 24 * 60 * 60 * 1000);
        let done_items = store.query_items(Some("done"), None, None, None);
        let recent = filter_recently_completed(store, &done_items, cutoff);

        let table = display::format_item_table(&recent);
        let history = if recent.is_empty() {
            "No items completed this week.\n".to_string()
        } else {
            format!("Completed this week ({}):\n{}", recent.len(), table)
        };
        serialize_response(&serde_json::json!({ "history": history }))
    } else {
        // Default: show 20 most recently completed items (not all 1000+)
        let since_ms = chrono::Utc::now().timestamp_millis() - (30 * 24 * 60 * 60 * 1000);
        let mut entries = nusy_kanban::stats::filter_history(
            store.items_batches(),
            store.runs_batches(),
            since_ms,
            None,
        );
        entries.truncate(20);
        let formatted = if entries.is_empty() {
            "No recently completed items.\n".to_string()
        } else {
            nusy_kanban::stats::format_history_entries(&entries)
        };
        serialize_response(&serde_json::json!({ "history": formatted }))
    }
}

/// Filter done items to those that transitioned to "done" after `cutoff_ms`.
///
/// Checks the runs table for `to_status == "done"` transitions. Falls back to
/// item creation timestamp if no matching run exists.
pub(crate) fn filter_recently_completed(
    store: &KanbanStore,
    done_items: &[arrow::array::RecordBatch],
    cutoff_ms: i64,
) -> Vec<arrow::array::RecordBatch> {
    use arrow::array::{Array, StringArray, TimestampMillisecondArray};
    use nusy_kanban::schema::{items_col, runs_col};

    // Build a set of item IDs that transitioned to "done" after cutoff
    let mut recent_ids = std::collections::HashSet::new();
    for batch in store.runs_batches() {
        let item_ids = batch
            .column(runs_col::ITEM_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("item_id");
        let to_statuses = batch
            .column(runs_col::TO_STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("to_status");
        let timestamps = batch
            .column(runs_col::TIMESTAMP)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .expect("timestamp");

        for i in 0..batch.num_rows() {
            if to_statuses.value(i) == "done"
                && !timestamps.is_null(i)
                && timestamps.value(i) > cutoff_ms
            {
                recent_ids.insert(item_ids.value(i).to_string());
            }
        }
    }

    // If runs table has matches, use those. Otherwise fall back to creation time.
    if !recent_ids.is_empty() {
        done_items
            .iter()
            .filter(|batch| {
                let ids = batch
                    .column(items_col::ID)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("id");
                (0..batch.num_rows()).any(|i| recent_ids.contains(ids.value(i)))
            })
            .cloned()
            .collect()
    } else {
        // Fallback: filter by creation timestamp (less accurate but works
        // when runs table is empty/sparse)
        done_items
            .iter()
            .filter(|batch| {
                let created = batch
                    .column(items_col::CREATED)
                    .as_any()
                    .downcast_ref::<TimestampMillisecondArray>()
                    .expect("created");
                (0..batch.num_rows()).any(|i| !created.is_null(i) && created.value(i) > cutoff_ms)
            })
            .cloned()
            .collect()
    }
}

// ── Blocked ─────────────────────────────────────────────────────────────────

pub(crate) fn handle_blocked(store: &KanbanStore) -> Result<Vec<u8>, Vec<u8>> {
    use arrow::array::{Array, BooleanArray, ListArray, StringArray};
    use nusy_kanban::schema::items_col;

    // Build set of done item IDs
    let mut done_ids = std::collections::HashSet::new();
    for batch in store.items_batches() {
        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id");
        let statuses = batch
            .column(items_col::STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("status");
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted");
        for i in 0..batch.num_rows() {
            if !deleted.value(i) && statuses.value(i) == "done" {
                done_ids.insert(ids.value(i).to_string());
            }
        }
    }

    // Filter to items with unmet dependencies
    let all = store.query_items(None, None, None, None);
    let blocked: Vec<arrow::array::RecordBatch> = all
        .into_iter()
        .filter(|batch| {
            let depends = batch
                .column(items_col::DEPENDS_ON)
                .as_any()
                .downcast_ref::<ListArray>()
                .expect("depends_on");
            let statuses = batch
                .column(items_col::STATUS)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("status");
            (0..batch.num_rows()).any(|i| {
                if statuses.value(i) == "done" {
                    return false;
                }
                if depends.is_null(i) || depends.value(i).is_empty() {
                    return false;
                }
                let dep_list = depends.value(i);
                let dep_strings = dep_list
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("dep values");
                (0..dep_strings.len()).any(|j| !done_ids.contains(dep_strings.value(j)))
            })
        })
        .collect();

    let table = display::format_item_table(&blocked);
    serialize_response(&serde_json::json!({
        "count": blocked.iter().map(|b| b.num_rows()).sum::<usize>(),
        "table": if blocked.is_empty() {
            "No blocked items.\n".to_string()
        } else {
            format!("Blocked Items ({}):\n{}", blocked.len(), table)
        },
    }))
}

// ── HDD Create ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct HddCreateRequest {
    title: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    related: Vec<String>,
    body: Option<String>,
    /// Paper number to link a hypothesis to (e.g. 130 for PAPER-130).
    paper: Option<u32>,
    /// Hypothesis ID to link an experiment to (e.g. H130.1).
    hypothesis: Option<String>,
    /// Experiment ID to link a measure to (optional, e.g. EXPR-130.1).
    experiment: Option<String>,
}

pub(crate) fn handle_hdd_create(
    payload: &[u8],
    store: &mut KanbanStore,
    relations: &mut RelationsStore,
    item_type: ItemType,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: HddCreateRequest = parse_payload(payload)?;

    // Each HDD-typed arm produces (id, body_needs_separate_update). The
    // typed helpers (create_paper/create_hypothesis/...) don't accept a
    // body, so the caller-supplied body has to be applied via update_body
    // after the create. The fallback arm sets body inline via
    // CreateItemInput, so it doesn't need a second pass.
    let (id, body_needs_update) = match item_type {
        ItemType::Paper => {
            let result =
                nusy_kanban::create_paper(store, &req.title, req.tags.clone(), req.related.clone())
                    .map_err(|e| error_response(&format!("{e}"), "HDD_CREATE_FAILED"))?;
            (result.id, true)
        }
        ItemType::Hypothesis => {
            let paper = req.paper.ok_or_else(|| {
                error_response(
                    "hdd.hypothesis requires `paper` (paper number to link to)",
                    "HDD_CREATE_FAILED",
                )
            })?;
            let result = nusy_kanban::create_hypothesis(
                store,
                relations,
                &req.title,
                paper,
                req.tags.clone(),
                req.related.clone(),
            )
            .map_err(|e| error_response(&format!("{e}"), "HDD_CREATE_FAILED"))?;
            (result.id, true)
        }
        ItemType::Experiment => {
            let hyp = req.hypothesis.as_deref().ok_or_else(|| {
                error_response(
                    "hdd.experiment requires `hypothesis` (hypothesis ID to link to)",
                    "HDD_CREATE_FAILED",
                )
            })?;
            let result = nusy_kanban::create_experiment(
                store,
                relations,
                &req.title,
                hyp,
                req.tags.clone(),
                req.related.clone(),
            )
            .map_err(|e| error_response(&format!("{e}"), "HDD_CREATE_FAILED"))?;
            (result.id, true)
        }
        ItemType::Measure => {
            let result = nusy_kanban::create_measure(
                store,
                relations,
                &req.title,
                req.experiment.as_deref(),
                req.tags.clone(),
                req.related.clone(),
            )
            .map_err(|e| error_response(&format!("{e}"), "HDD_CREATE_FAILED"))?;
            (result.id, true)
        }
        ItemType::Idea => {
            let result =
                nusy_kanban::create_idea(store, &req.title, req.tags.clone(), req.related.clone())
                    .map_err(|e| error_response(&format!("{e}"), "HDD_CREATE_FAILED"))?;
            (result.id, true)
        }
        ItemType::Literature => {
            let result = nusy_kanban::create_literature(
                store,
                &req.title,
                req.tags.clone(),
                req.related.clone(),
            )
            .map_err(|e| error_response(&format!("{e}"), "HDD_CREATE_FAILED"))?;
            (result.id, true)
        }
        _ => {
            let input = CreateItemInput {
                title: req.title.clone(),
                item_type,
                priority: None,
                assignee: None,
                tags: req.tags,
                related: req.related,
                depends_on: vec![],
                body: req.body.clone(),
            };
            let id = store
                .create_item(&input)
                .map_err(|e| error_response(&format!("{e}"), "HDD_CREATE_FAILED"))?;
            (id, false)
        }
    };

    if body_needs_update
        && let Some(body) = req.body.as_deref()
        && let Err(e) = store.update_body(&id, Some(body))
    {
        return Err(error_response(
            &format!("created {id} but failed to set body: {e}"),
            "HDD_CREATE_FAILED",
        ));
    }

    serialize_response(&CreateResponse {
        id,
        title: req.title,
        item_type: item_type.as_str().to_string(),
        status: "backlog".to_string(),
    })
}

// ── HDD Validate ────────────────────────────────────────────────────────────

pub(crate) fn handle_hdd_validate(
    store: &KanbanStore,
    relations: &RelationsStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let result = nusy_kanban::validate_hdd(store, relations);
    serialize_response(&serde_json::json!({
        "valid": result.is_empty(),
        "issues": result,
    }))
}

// ── HDD Registry ────────────────────────────────────────────────────────────

pub(crate) fn handle_hdd_registry(
    store: &KanbanStore,
    relations: &RelationsStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let chains = nusy_kanban::build_registry(store, relations);

    // Manually serialize since RegistryChain doesn't derive Serialize
    let registry: Vec<serde_json::Value> = chains
        .iter()
        .map(|chain| {
            serde_json::json!({
                "paper_id": chain.paper_id,
                "paper_title": chain.paper_title,
                "hypotheses": chain.hypotheses.iter().map(|h| {
                    serde_json::json!({
                        "id": h.id,
                        "title": h.title,
                        "experiments": h.experiments.iter().map(|e| {
                            serde_json::json!({
                                "id": e.id,
                                "title": e.title,
                            })
                        }).collect::<Vec<_>>(),
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();

    serialize_response(&serde_json::json!({ "registry": registry }))
}

// ── Relation Add ────────────────────────────────────────────────────────────
pub(crate) fn handle_templates(payload: &[u8], root: &std::path::Path) -> Result<Vec<u8>, Vec<u8>> {
    let params: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| error_response(&e.to_string(), "INVALID_JSON"))?;

    let loader = nusy_kanban::templates::ShapeLoader::new(root);
    let generator = nusy_kanban::templates::TemplateGenerator::new(loader);

    if let Some(type_str) = params.get("item_type").and_then(|v| v.as_str()) {
        if let Some(it) = nusy_kanban::ItemType::from_str_loose(type_str) {
            let template = generator.generate(&it, "<Title>");
            serialize_response(&serde_json::json!({ "template": template }))
        } else {
            Err(error_response(
                &format!("Unknown item type: {type_str}"),
                "UNKNOWN_TYPE",
            ))
        }
    } else {
        let summaries = generator.list_all();
        let types: Vec<serde_json::Value> = summaries
            .iter()
            .map(|s| {
                serde_json::json!({
                    "type": s.item_type.as_str(),
                    "description": s.description
                })
            })
            .collect();
        serialize_response(&serde_json::json!({ "types": types }))
    }
}
