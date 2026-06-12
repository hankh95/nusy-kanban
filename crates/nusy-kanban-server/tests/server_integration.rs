//! Integration tests for nusy-kanban-server — handler dispatch without NATS.
//!
//! Tests exercise the dispatch() function directly, bypassing NATS transport.
//! This validates the full request→handler→response pipeline with persistence.
//!
//! EXP-3002 Phase 3.

use nusy_kanban_server::events::detect_mutation;
use nusy_kanban_server::handlers::dispatch;
use nusy_kanban_server::state::ServerState;

fn test_state(dir: &std::path::Path) -> ServerState {
    ServerState {
        store: nusy_kanban::crud::KanbanStore::new(),
        relations: nusy_kanban::relations::RelationsStore::new(),
        #[cfg(feature = "pr")]
        proposals: nusy_graph_review::ProposalStore::new(),
        #[cfg(feature = "pr")]
        comments: nusy_graph_review::CommentStore::new(),
        #[cfg(feature = "pr")]
        ci_results: nusy_graph_review::CiResultStore::new(),
        data_dir: dir.to_path_buf(),
    }
}

// ─── Create + Show + List Lifecycle ─────────────────────────────────────────

#[test]
fn test_create_show_list_lifecycle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Create
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "Integration test expedition",
        "item_type": "expedition",
        "priority": "high",
        "tags": ["integration-test", "v14"]
    }))
    .unwrap();

    let resp = dispatch("kanban.cmd.create", &payload, &mut state);
    let created: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(created.get("error").is_none(), "no error on create");
    let id = created["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("EX-"), "ID has EX prefix");
    assert_eq!(created["status"], "backlog");

    // Show
    let show_payload = serde_json::to_vec(&serde_json::json!({ "id": id })).unwrap();
    let show_resp = dispatch("kanban.cmd.show", &show_payload, &mut state);
    let shown: serde_json::Value = serde_json::from_slice(&show_resp).unwrap();
    assert_eq!(shown["id"], id, "show returns correct ID");
    assert!(shown.get("detail").is_some(), "show returns detail field");

    // List
    let list_payload = serde_json::to_vec(&serde_json::json!({ "status": "backlog" })).unwrap();
    let list_resp = dispatch("kanban.cmd.list", &list_payload, &mut state);
    let listed: serde_json::Value = serde_json::from_slice(&list_resp).unwrap();
    assert!(listed.get("count").is_some(), "list returns count");
    assert!(
        listed["count"].as_u64().unwrap() >= 1,
        "at least one item in list"
    );
}

// ─── Move + History ─────────────────────────────────────────────────────────

#[test]
fn test_move_and_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Create item
    let create = serde_json::to_vec(&serde_json::json!({
        "title": "Move test",
        "item_type": "chore",
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.create", &create, &mut state);
    let created: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    let id = created["id"].as_str().unwrap();

    // Move to in_progress
    let move_payload = serde_json::to_vec(&serde_json::json!({
        "id": id,
        "status": "in_progress",
        "assignee": "M5"
    }))
    .unwrap();
    let move_resp = dispatch("kanban.cmd.move", &move_payload, &mut state);
    let moved: serde_json::Value = serde_json::from_slice(&move_resp).unwrap();
    assert!(moved.get("error").is_none(), "no error on move");

    // History
    let hist_payload = serde_json::to_vec(&serde_json::json!({ "id": id })).unwrap();
    let hist_resp = dispatch("kanban.cmd.history", &hist_payload, &mut state);
    let history: serde_json::Value = serde_json::from_slice(&hist_resp).unwrap();
    assert!(
        history.get("history").is_some(),
        "history returns history field"
    );
}

// ─── Concurrent Sequential Creates Get Unique IDs ───────────────────────────

#[test]
fn test_sequential_creates_unique_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let mut ids = Vec::new();
    for i in 0..10 {
        let payload = serde_json::to_vec(&serde_json::json!({
            "title": format!("Item {i}"),
            "item_type": "expedition",
        }))
        .unwrap();
        let resp = dispatch("kanban.cmd.create", &payload, &mut state);
        let created: serde_json::Value = serde_json::from_slice(&resp).unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }

    // All IDs should be unique
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 10, "all 10 IDs unique");

    // IDs should be sequential
    for window in ids.windows(2) {
        let a: u32 = window[0].strip_prefix("EX-").unwrap().parse().unwrap();
        let b: u32 = window[1].strip_prefix("EX-").unwrap().parse().unwrap();
        assert_eq!(b, a + 1, "IDs are sequential");
    }
}

// ─── Update ─────────────────────────────────────────────────────────────────

#[test]
fn test_update_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Create
    let create = serde_json::to_vec(&serde_json::json!({
        "title": "Original title",
        "item_type": "expedition",
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.create", &create, &mut state);
    let created: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    let id = created["id"].as_str().unwrap();

    // Update title and priority
    let update = serde_json::to_vec(&serde_json::json!({
        "id": id,
        "title": "Updated title",
        "priority": "critical",
        "tags": ["updated", "v14"]
    }))
    .unwrap();
    let update_resp = dispatch("kanban.cmd.update", &update, &mut state);
    let updated: serde_json::Value = serde_json::from_slice(&update_resp).unwrap();
    assert!(updated.get("error").is_none(), "no error on update");

    // Verify via show
    let show = serde_json::to_vec(&serde_json::json!({ "id": id })).unwrap();
    let show_resp = dispatch("kanban.cmd.show", &show, &mut state);
    let shown: serde_json::Value = serde_json::from_slice(&show_resp).unwrap();
    assert_eq!(shown["id"], id, "show returns correct ID");
    // The detail field contains the rendered item which should include the updated title
    let detail = shown["detail"].as_str().unwrap_or("");
    assert!(
        detail.contains("Updated title"),
        "detail should contain updated title"
    );
}

// ─── Mutation Events ────────────────────────────────────────────────────────

#[test]
fn test_create_emits_mutation_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "Event test",
        "item_type": "expedition",
    }))
    .unwrap();

    let resp = dispatch("kanban.cmd.create", &payload, &mut state);

    let created: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(created.get("error").is_none());
    let event = detect_mutation("create", &resp);
    assert!(event.is_some(), "create should emit mutation event");
}

// ─── Error Handling ─────────────────────────────────────────────────────────

#[test]
fn test_show_nonexistent_returns_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let payload = serde_json::to_vec(&serde_json::json!({ "id": "EXP-99999" })).unwrap();
    let resp = dispatch("kanban.cmd.show", &payload, &mut state);
    let result: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(result.get("error").is_some(), "should return error");
}

#[test]
fn test_invalid_command_returns_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let resp = dispatch("kanban.cmd.nonexistent", b"{}", &mut state);
    let result: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(
        result.get("error").is_some(),
        "unknown command returns error"
    );
}

// ─── Relations via Server ───────────────────────────────────────────────────

#[test]
fn test_relation_add_and_query() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Create two items
    let create1 = serde_json::to_vec(&serde_json::json!({
        "title": "Source item",
        "item_type": "expedition",
    }))
    .unwrap();
    let resp1 = dispatch("kanban.cmd.create", &create1, &mut state);
    let id1: serde_json::Value = serde_json::from_slice(&resp1).unwrap();

    let create2 = serde_json::to_vec(&serde_json::json!({
        "title": "Target item",
        "item_type": "expedition",
    }))
    .unwrap();
    let resp2 = dispatch("kanban.cmd.create", &create2, &mut state);
    let id2: serde_json::Value = serde_json::from_slice(&resp2).unwrap();

    // Add relation
    let rel_payload = serde_json::to_vec(&serde_json::json!({
        "source_id": id1["id"],
        "target_id": id2["id"],
        "predicate": "blocks"
    }))
    .unwrap();
    let rel_resp = dispatch("kanban.cmd.relation.add", &rel_payload, &mut state);
    let rel_result: serde_json::Value = serde_json::from_slice(&rel_resp).unwrap();
    assert!(rel_result.get("error").is_none(), "relation created");
    assert!(rel_result.get("relation_id").is_some(), "relation has ID");
}

// ─── HDD (Research Board) via Server ────────────────────────────────────────

#[test]
fn test_hdd_paper_create() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "Arrow-Native Reasoning Performance",
        "tags": ["arrow", "benchmark"]
    }))
    .unwrap();

    let resp = dispatch("kanban.cmd.hdd.paper", &payload, &mut state);
    let result: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(result.get("error").is_none(), "paper created");
    let id = result["id"].as_str().unwrap();
    assert!(id.starts_with("PAPER-"), "paper ID has PAPER prefix: {id}");
}

/// Helper: create a paper via the server and return its ID + numeric paper number.
fn create_paper_for_test(state: &mut ServerState, title: &str) -> (String, u32) {
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": title,
        "tags": ["test"]
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.hdd.paper", &payload, state);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    let id = v["id"].as_str().unwrap().to_string();
    let num: u32 = id
        .strip_prefix("PAPER-")
        .and_then(|s| s.parse().ok())
        .expect("paper id is PAPER-<u32>");
    (id, num)
}

/// Show an item via the server and parse the returned JSON body.
/// Returns the inner JSON (already an object) for the item.
fn show_item_json(state: &mut ServerState, id: &str) -> serde_json::Value {
    let payload = serde_json::to_vec(&serde_json::json!({ "id": id, "format": "json" })).unwrap();
    let resp = dispatch("kanban.cmd.show", &payload, state);
    let wrapper: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    let json_str = wrapper["json"]
        .as_str()
        .expect("show format=json returns `json` string");
    let parsed: serde_json::Value = serde_json::from_str(json_str).expect("inner json parses");
    // export_json returns a JSON array; we want the single matching item.
    let arr = parsed.as_array().expect("inner json is an array");
    arr.iter()
        .find(|item| item["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no item with id={id} in show output: {parsed}"))
}

#[test]
fn test_hdd_hypothesis_paper_scoped_id_and_link() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let (paper_id, paper_num) = create_paper_for_test(&mut state, "Carrier Paper");

    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "Embedding geometry mirrors graph distance within ε",
        "paper": paper_num,
        "tags": ["test"]
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.hdd.hypothesis", &payload, &mut state);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(v.get("error").is_none(), "hypothesis created: {v}");
    let hid = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        hid,
        format!("H{paper_num}.1"),
        "hypothesis gets paper-scoped ID H<paper>.<seq>, got: {hid}"
    );

    // Second hypothesis under the same paper bumps the sequence to .2
    let payload2 = serde_json::to_vec(&serde_json::json!({
        "title": "Second claim",
        "paper": paper_num,
    }))
    .unwrap();
    let resp2 = dispatch("kanban.cmd.hdd.hypothesis", &payload2, &mut state);
    let v2: serde_json::Value = serde_json::from_slice(&resp2).unwrap();
    let hid2 = v2["id"].as_str().unwrap();
    assert_eq!(hid2, format!("H{paper_num}.2"), "sequence increments");

    // Auto-link: the hypothesis's `related` field must contain the paper.
    let item = show_item_json(&mut state, &hid);
    let related: Vec<&str> = item["related"]
        .as_array()
        .expect("related array")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    assert!(
        related.contains(&paper_id.as_str()),
        "hypothesis auto-linked to paper in `related`; got {related:?}"
    );

    // The "tests" predicate must exist in the relations store. Inspect directly.
    let rel_batches = state.relations.query_relations(&hid);
    let total_rows: usize = rel_batches.iter().map(|b| b.num_rows()).sum();
    assert!(
        total_rows >= 1,
        "at least one relation edge from hyp ({hid}); got {total_rows}"
    );
}

#[test]
fn test_hdd_hypothesis_missing_paper_field_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // No `paper` field — must reject, not silently create with a global ID.
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "Hypothesis with no paper link",
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.hdd.hypothesis", &payload, &mut state);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(
        v.get("error").is_some(),
        "missing `paper` is rejected; got: {v}"
    );
}

#[test]
fn test_hdd_experiment_paper_scoped_id_and_link() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let (_paper_id, paper_num) = create_paper_for_test(&mut state, "Carrier Paper");

    // Create the hypothesis the experiment will link to.
    let hyp_payload = serde_json::to_vec(&serde_json::json!({
        "title": "Target hypothesis",
        "paper": paper_num,
    }))
    .unwrap();
    let hyp_resp = dispatch("kanban.cmd.hdd.hypothesis", &hyp_payload, &mut state);
    let hyp_v: serde_json::Value = serde_json::from_slice(&hyp_resp).unwrap();
    let hyp_id = hyp_v["id"].as_str().unwrap().to_string();

    // Experiment links to the hypothesis.
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "A/B fastembed vs graph traversal",
        "hypothesis": hyp_id,
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.hdd.experiment", &payload, &mut state);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(v.get("error").is_none(), "experiment created: {v}");
    let eid = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        eid,
        format!("EXPR-{paper_num}.1"),
        "experiment gets paper-scoped ID EXPR-<paper>.<seq>, got: {eid}"
    );

    // Auto-link: experiment's `related` includes the hypothesis.
    let item = show_item_json(&mut state, &eid);
    let related: Vec<&str> = item["related"]
        .as_array()
        .expect("related array")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    assert!(
        related.contains(&hyp_id.as_str()),
        "experiment auto-linked to hypothesis in `related`; got {related:?}"
    );

    // "validates" predicate edge exists in the relations store.
    let rel_batches = state.relations.query_relations(&eid);
    let total_rows: usize = rel_batches.iter().map(|b| b.num_rows()).sum();
    assert!(
        total_rows >= 1,
        "at least one relation edge from experiment ({eid}); got {total_rows}"
    );
}

#[test]
fn test_hdd_experiment_missing_hypothesis_field_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "Experiment with no hypothesis link",
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.hdd.experiment", &payload, &mut state);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(
        v.get("error").is_some(),
        "missing `hypothesis` is rejected; got: {v}"
    );
}

#[test]
fn test_hdd_measure_with_and_without_experiment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let (_paper_id, paper_num) = create_paper_for_test(&mut state, "Paper");

    let hyp_payload = serde_json::to_vec(&serde_json::json!({
        "title": "H",
        "paper": paper_num,
    }))
    .unwrap();
    let hyp_id = serde_json::from_slice::<serde_json::Value>(&dispatch(
        "kanban.cmd.hdd.hypothesis",
        &hyp_payload,
        &mut state,
    ))
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let exp_payload = serde_json::to_vec(&serde_json::json!({
        "title": "E",
        "hypothesis": hyp_id,
    }))
    .unwrap();
    let exp_id = serde_json::from_slice::<serde_json::Value>(&dispatch(
        "kanban.cmd.hdd.experiment",
        &exp_payload,
        &mut state,
    ))
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Linked measure → `related` includes the experiment + predicate edge exists.
    let linked_payload = serde_json::to_vec(&serde_json::json!({
        "title": "Latency",
        "experiment": exp_id,
    }))
    .unwrap();
    let linked_resp = dispatch("kanban.cmd.hdd.measure", &linked_payload, &mut state);
    let linked_v: serde_json::Value = serde_json::from_slice(&linked_resp).unwrap();
    assert!(linked_v.get("error").is_none(), "linked measure created");
    let m_id = linked_v["id"].as_str().unwrap().to_string();

    let item = show_item_json(&mut state, &m_id);
    let related: Vec<&str> = item["related"]
        .as_array()
        .expect("related array")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    assert!(
        related.contains(&exp_id.as_str()),
        "measure auto-linked to experiment; got {related:?}"
    );

    let rel_batches = state.relations.query_relations(&m_id);
    let total_rows: usize = rel_batches.iter().map(|b| b.num_rows()).sum();
    assert!(
        total_rows >= 1,
        "at least one relation edge from measure when experiment supplied; got {total_rows}"
    );

    // Standalone measure (no `experiment`) — succeeds, no auto-link, empty related.
    let stand_payload = serde_json::to_vec(&serde_json::json!({
        "title": "Standalone metric",
    }))
    .unwrap();
    let stand_resp = dispatch("kanban.cmd.hdd.measure", &stand_payload, &mut state);
    let stand_v: serde_json::Value = serde_json::from_slice(&stand_resp).unwrap();
    assert!(stand_v.get("error").is_none(), "standalone measure created");
    let stand_id = stand_v["id"].as_str().unwrap().to_string();

    let stand_item = show_item_json(&mut state, &stand_id);
    let stand_related = stand_item["related"]
        .as_array()
        .expect("related array")
        .len();
    assert_eq!(
        stand_related, 0,
        "standalone measure has no auto-linked experiment"
    );
}

#[test]
fn test_hdd_create_persists_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let (_paper_id, paper_num) = create_paper_for_test(&mut state, "Paper");

    let body = "## Claim\n\nThe latency target is >=15%.\n\n## Falsifiable By\n\nDelta below 0%.";
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "Body-bearing hypothesis",
        "paper": paper_num,
        "body": body,
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.hdd.hypothesis", &payload, &mut state);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(v.get("error").is_none(), "hypothesis created: {v}");
    let hid = v["id"].as_str().unwrap().to_string();

    // Body must round-trip through the two-step create+update_body path.
    let item = show_item_json(&mut state, &hid);
    assert_eq!(
        item["body"].as_str(),
        Some(body),
        "body round-trips; got: {item}"
    );
}

#[test]
fn test_hdd_create_forwards_related() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let (_paper_id, paper_num) = create_paper_for_test(&mut state, "Paper");

    // Create a sibling expedition to point at.
    let sibling_payload = serde_json::to_vec(&serde_json::json!({
        "title": "Implementation expedition",
        "item_type": "expedition",
    }))
    .unwrap();
    let sibling_resp = dispatch("kanban.cmd.create", &sibling_payload, &mut state);
    let sibling_id = serde_json::from_slice::<serde_json::Value>(&sibling_resp).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Hypothesis with a caller-supplied related list — must end up in the item's related field.
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "Hyp with extra related",
        "paper": paper_num,
        "related": [sibling_id.clone()],
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.hdd.hypothesis", &payload, &mut state);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(v.get("error").is_none(), "hypothesis created: {v}");
    let hid = v["id"].as_str().unwrap().to_string();

    // Related must include both the auto-linked paper AND the caller-supplied expedition.
    let item = show_item_json(&mut state, &hid);
    let related = item["related"]
        .as_array()
        .expect("related is an array")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        related.iter().any(|r| r == &format!("PAPER-{paper_num}")),
        "auto-linked paper still present in related; got {related:?}"
    );
    assert!(
        related.contains(&sibling_id),
        "caller-supplied related forwarded; got {related:?}"
    );
}

// ─── Blocked Items via Server ───────────────────────────────────────────────

#[test]
fn test_blocked_via_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Create two items, second depends on first
    let create1 = serde_json::to_vec(&serde_json::json!({
        "title": "Dependency",
        "item_type": "expedition",
    }))
    .unwrap();
    let resp1 = dispatch("kanban.cmd.create", &create1, &mut state);
    let id1 = serde_json::from_slice::<serde_json::Value>(&resp1).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let create2 = serde_json::to_vec(&serde_json::json!({
        "title": "Blocked by dependency",
        "item_type": "expedition",
        "depends_on": [&id1]
    }))
    .unwrap();
    dispatch("kanban.cmd.create", &create2, &mut state);

    let blocked_resp = dispatch("kanban.cmd.blocked", b"{}", &mut state);
    let blocked: serde_json::Value = serde_json::from_slice(&blocked_resp).unwrap();
    assert!(blocked.get("error").is_none(), "blocked command succeeds");
}

// ── Git command dispatch tests (EX-3012) ────────────────────────────────────

#[test]
fn test_git_push_returns_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let resp = dispatch("kanban.cmd.git.push", b"{}", &mut state);
    let result: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(result.get("error").is_none(), "git.push should not error");
    let msg = result["message"].as_str().unwrap();
    assert!(msg.contains("push"), "message mentions push: {msg}");
}

#[test]
fn test_git_pull_returns_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let resp = dispatch("kanban.cmd.git.pull", b"{}", &mut state);
    let result: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(result.get("error").is_none(), "git.pull should not error");
    assert!(result["message"].as_str().unwrap().contains("pull"));
}

#[test]
fn test_git_clone_returns_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let resp = dispatch("kanban.cmd.git.clone", b"{}", &mut state);
    let result: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(result.get("error").is_none(), "git.clone should not error");
    assert!(result["message"].as_str().unwrap().contains("clone"));
}

#[test]
fn test_git_log_returns_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let resp = dispatch("kanban.cmd.git.log", b"{}", &mut state);
    let result: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(result.get("error").is_none(), "git.log should not error");
    assert!(result["detail"].as_str().unwrap().contains("log"));
}

#[test]
fn test_git_blame_returns_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let resp = dispatch("kanban.cmd.git.blame", b"{}", &mut state);
    let result: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(result.get("error").is_none(), "git.blame should not error");
    assert!(result["detail"].as_str().unwrap().contains("blame"));
}

#[test]
fn test_git_rebase_returns_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let resp = dispatch("kanban.cmd.git.rebase", b"{}", &mut state);
    let result: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(result.get("error").is_none(), "git.rebase should not error");
    assert!(result["detail"].as_str().unwrap().contains("rebase"));
}

// ─── Move with Resolution + ClosedBy (EX-3081) ─────────────────────────────

#[test]
fn test_move_with_resolution_and_closed_by() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Create item
    let create = serde_json::to_vec(&serde_json::json!({
        "title": "Resolution test",
        "item_type": "expedition",
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.create", &create, &mut state);
    let created: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    let id = created["id"].as_str().unwrap();

    // Move to done with resolution + closed_by
    let move_payload = serde_json::to_vec(&serde_json::json!({
        "id": id,
        "status": "done",
        "resolution": "wont_do",
        "closed_by": "PROP-2099",
    }))
    .unwrap();
    let move_resp = dispatch("kanban.cmd.move", &move_payload, &mut state);
    let moved: serde_json::Value = serde_json::from_slice(&move_resp).unwrap();
    assert!(moved.get("error").is_none(), "no error on move: {moved:?}");
    assert_eq!(moved["to"], "done");
    assert_eq!(moved["resolution"], "wont_do");

    // Verify via show — detail should contain resolution and closed_by
    let show_payload = serde_json::to_vec(&serde_json::json!({ "id": id })).unwrap();
    let show_resp = dispatch("kanban.cmd.show", &show_payload, &mut state);
    let shown: serde_json::Value = serde_json::from_slice(&show_resp).unwrap();
    let detail = shown["detail"].as_str().unwrap_or("");
    assert!(
        detail.contains("wont_do"),
        "detail shows resolution: {detail}"
    );
    assert!(
        detail.contains("PROP-2099"),
        "detail shows closed_by: {detail}"
    );
}

#[test]
fn test_move_with_invalid_resolution_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Create item
    let create = serde_json::to_vec(&serde_json::json!({
        "title": "Invalid resolution test",
        "item_type": "expedition",
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.create", &create, &mut state);
    let created: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    let id = created["id"].as_str().unwrap();

    // Move with invalid resolution — should fail
    let move_payload = serde_json::to_vec(&serde_json::json!({
        "id": id,
        "status": "done",
        "resolution": "cancelled",
    }))
    .unwrap();
    let move_resp = dispatch("kanban.cmd.move", &move_payload, &mut state);
    let result: serde_json::Value = serde_json::from_slice(&move_resp).unwrap();
    assert!(
        result.get("error").is_some(),
        "invalid resolution should error"
    );
}

#[test]
fn test_resolution_on_non_terminal_state_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Create item
    let create = serde_json::to_vec(&serde_json::json!({
        "title": "Non-terminal resolution test",
        "item_type": "expedition",
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.create", &create, &mut state);
    let created: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    let id = created["id"].as_str().unwrap();

    // Move to in_progress with resolution — should fail
    let move_payload = serde_json::to_vec(&serde_json::json!({
        "id": id,
        "status": "in_progress",
        "resolution": "completed",
    }))
    .unwrap();
    let move_resp = dispatch("kanban.cmd.move", &move_payload, &mut state);
    let result: serde_json::Value = serde_json::from_slice(&move_resp).unwrap();
    assert!(
        result.get("error").is_some(),
        "resolution on non-terminal should error"
    );
}

// ─── CH-4307: list filters (tag, priority, resolution) ─────────────────────

/// Helper: create an item via dispatch and return its ID.
fn create_item(state: &mut ServerState, body: serde_json::Value) -> String {
    let payload = serde_json::to_vec(&body).unwrap();
    let resp = dispatch("kanban.cmd.create", &payload, state);
    let created: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    created["id"].as_str().unwrap().to_string()
}

/// CH-4307: `nk list --tag X` previously returned the full board because
/// `ListRequest` on the server side didn't include `tags` and the field was
/// silently dropped by serde. Now the filter actually works.
#[test]
fn test_list_filter_by_single_tag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let target_id = create_item(
        &mut state,
        serde_json::json!({
            "title": "Tagged with v12-parity",
            "item_type": "chore",
            "tags": ["v12-parity", "kanban"],
        }),
    );
    let _other_id = create_item(
        &mut state,
        serde_json::json!({
            "title": "No matching tag",
            "item_type": "chore",
            "tags": ["unrelated"],
        }),
    );
    let _untagged_id = create_item(
        &mut state,
        serde_json::json!({ "title": "No tags at all", "item_type": "chore" }),
    );

    let payload = serde_json::to_vec(&serde_json::json!({ "tags": ["v12-parity"] })).unwrap();
    let resp = dispatch("kanban.cmd.list", &payload, &mut state);
    let listed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(
        listed["count"], 1,
        "only the v12-parity item should match, got {listed}"
    );
    let table = listed["table"].as_str().unwrap();
    assert!(
        table.contains(&target_id),
        "filtered table should contain the matching item id {target_id}"
    );
}

/// Multiple `--tag` flags AND together (per the CLI's --help text).
#[test]
fn test_list_filter_multiple_tags_and_logic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let both = create_item(
        &mut state,
        serde_json::json!({
            "title": "Has both tags",
            "item_type": "chore",
            "tags": ["v12-parity", "kanban"],
        }),
    );
    let _only_one = create_item(
        &mut state,
        serde_json::json!({
            "title": "Has only v12-parity",
            "item_type": "chore",
            "tags": ["v12-parity"],
        }),
    );

    let payload = serde_json::to_vec(&serde_json::json!({
        "tags": ["v12-parity", "kanban"],
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.list", &payload, &mut state);
    let listed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(
        listed["count"], 1,
        "AND-logic should require BOTH tags, got {listed}"
    );
    assert!(listed["table"].as_str().unwrap().contains(&both));
}

/// Unknown tag returns zero items, not the full board.
#[test]
fn test_list_filter_unknown_tag_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    create_item(
        &mut state,
        serde_json::json!({
            "title": "Some tagged item",
            "item_type": "chore",
            "tags": ["v12-parity"],
        }),
    );
    create_item(
        &mut state,
        serde_json::json!({ "title": "Untagged item", "item_type": "chore" }),
    );

    let payload = serde_json::to_vec(&serde_json::json!({ "tags": ["nonexistent_xyz"] })).unwrap();
    let resp = dispatch("kanban.cmd.list", &payload, &mut state);
    let listed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(
        listed["count"], 0,
        "unknown tag must return 0 items, not the full board"
    );
}

/// Priority filter applies on the server side too (was on `ListRequest` neither
/// before CH-4307; we add it alongside the tag fix to keep parity with local
/// mode).
#[test]
fn test_list_filter_by_priority() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let high_id = create_item(
        &mut state,
        serde_json::json!({
            "title": "High priority",
            "item_type": "chore",
            "priority": "high",
        }),
    );
    create_item(
        &mut state,
        serde_json::json!({
            "title": "Low priority",
            "item_type": "chore",
            "priority": "low",
        }),
    );

    let payload = serde_json::to_vec(&serde_json::json!({ "priority": "high" })).unwrap();
    let resp = dispatch("kanban.cmd.list", &payload, &mut state);
    let listed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(
        listed["count"], 1,
        "only the high-priority item should match"
    );
    assert!(listed["table"].as_str().unwrap().contains(&high_id));
}

/// Resolution filter applies on the server side. Move two items to terminal
/// states with different resolutions, then filter.
#[test]
fn test_list_filter_by_resolution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let completed = create_item(
        &mut state,
        serde_json::json!({ "title": "To be completed", "item_type": "chore" }),
    );
    let superseded = create_item(
        &mut state,
        serde_json::json!({ "title": "To be superseded", "item_type": "chore" }),
    );

    // Move both to done with different resolutions.
    for (id, res) in [(&completed, "completed"), (&superseded, "superseded")] {
        let move_payload = serde_json::to_vec(&serde_json::json!({
            "id": id,
            "status": "done",
            "resolution": res,
        }))
        .unwrap();
        let move_resp = dispatch("kanban.cmd.move", &move_payload, &mut state);
        let result: serde_json::Value = serde_json::from_slice(&move_resp).unwrap();
        assert!(
            result.get("error").is_none(),
            "move {id} → done with resolution={res} should succeed: {result}"
        );
    }

    let payload = serde_json::to_vec(&serde_json::json!({
        "status": "done",
        "resolution": "superseded",
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.list", &payload, &mut state);
    let listed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(listed["count"], 1, "only the superseded item should match");
    let table = listed["table"].as_str().unwrap();
    assert!(table.contains(&superseded));
    assert!(!table.contains(&completed));
}

/// Tag filter combines correctly with the existing status filter — the same
/// item must satisfy BOTH gates.
#[test]
fn test_list_filter_tag_and_status_combined() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Two items with the tag, one of which is moved to in_progress.
    let backlog_id = create_item(
        &mut state,
        serde_json::json!({
            "title": "Tagged, backlog",
            "item_type": "chore",
            "tags": ["v12-parity"],
        }),
    );
    let in_progress_id = create_item(
        &mut state,
        serde_json::json!({
            "title": "Tagged, in_progress",
            "item_type": "chore",
            "tags": ["v12-parity"],
        }),
    );
    let move_payload = serde_json::to_vec(&serde_json::json!({
        "id": &in_progress_id,
        "status": "in_progress",
    }))
    .unwrap();
    let _ = dispatch("kanban.cmd.move", &move_payload, &mut state);

    // Filter by status=in_progress AND tag=v12-parity → only the moved item.
    let payload = serde_json::to_vec(&serde_json::json!({
        "status": "in_progress",
        "tags": ["v12-parity"],
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.list", &payload, &mut state);
    let listed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(listed["count"], 1);
    let table = listed["table"].as_str().unwrap();
    assert!(table.contains(&in_progress_id));
    assert!(!table.contains(&backlog_id));
}

// ─── CH-4521: Rank dispatch ────────────────────────────────────────────────

#[test]
fn test_rank_dispatch_sets_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let id = create_item(
        &mut state,
        serde_json::json!({
            "title": "Item to rank",
            "item_type": "expedition",
        }),
    );

    // Initial show: rank should be null
    let item = show_item_json(&mut state, &id);
    assert!(item["rank"].is_null(), "fresh item starts unranked");

    // Set rank=1
    let rank_payload = serde_json::to_vec(&serde_json::json!({
        "id": &id,
        "rank": 1,
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.rank", &rank_payload, &mut state);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(v.get("error").is_none(), "rank command succeeds: {v}");
    assert_eq!(v["id"], id);
    assert_eq!(v["rank"], 1);

    // Re-show: rank now set
    let item2 = show_item_json(&mut state, &id);
    assert_eq!(item2["rank"].as_i64(), Some(1), "rank persisted: {item2}");

    // Update to a different rank
    let rank2 = serde_json::to_vec(&serde_json::json!({ "id": &id, "rank": 5 })).unwrap();
    dispatch("kanban.cmd.rank", &rank2, &mut state);
    let item3 = show_item_json(&mut state, &id);
    assert_eq!(item3["rank"].as_i64(), Some(5));

    // Clear rank (null)
    let clear = serde_json::to_vec(&serde_json::json!({ "id": &id, "rank": null })).unwrap();
    dispatch("kanban.cmd.rank", &clear, &mut state);
    let item4 = show_item_json(&mut state, &id);
    assert!(item4["rank"].is_null(), "rank cleared: {item4}");
}

#[test]
fn test_rank_dispatch_nonexistent_id_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let payload = serde_json::to_vec(&serde_json::json!({
        "id": "EX-NOPE",
        "rank": 1,
    }))
    .unwrap();
    let resp = dispatch("kanban.cmd.rank", &payload, &mut state);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert!(
        v.get("error").is_some(),
        "missing item must error, not silently no-op: {v}"
    );
}

#[test]
fn test_rank_does_not_overwrite_priority() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let id = create_item(
        &mut state,
        serde_json::json!({
            "title": "Critical item",
            "item_type": "expedition",
            "priority": "critical",
        }),
    );
    let rank_payload = serde_json::to_vec(&serde_json::json!({ "id": &id, "rank": 1 })).unwrap();
    dispatch("kanban.cmd.rank", &rank_payload, &mut state);
    let item = show_item_json(&mut state, &id);
    assert_eq!(item["priority"], "critical", "priority unchanged");
    assert_eq!(item["rank"].as_i64(), Some(1), "rank set");
}
