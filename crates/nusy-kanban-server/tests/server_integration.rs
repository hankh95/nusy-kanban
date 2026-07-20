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
        health: nusy_kanban_server::health::HealthGate::new(),
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

// ─── CH-6056: write-durability gate (HZ-6053) ───────────────────────────────
//
// The defect these cover: the server used to apply a mutation to memory, fail
// to persist it, log "Warning: ...", and still return SUCCESS. Agents were told
// writes landed that a restart would erase.

/// Make a directory reject new files, simulating a store that cannot be written.
#[cfg(unix)]
fn make_unwritable(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(dir).expect("meta").permissions();
    perms.set_mode(0o500);
    std::fs::set_permissions(dir, perms).expect("chmod");
}

#[cfg(unix)]
fn make_writable(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(dir).expect("meta").permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(dir, perms).expect("chmod");
}

fn response_json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or(serde_json::Value::Null)
}

/// A create whose persist fails must NOT come back as success.
#[cfg(unix)]
#[test]
fn test_unpersistable_mutation_is_reported_as_an_error_not_acked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    make_unwritable(dir.path());
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "write that cannot be kept",
        "item_type": "chore",
    }))
    .unwrap();
    let resp = response_json(&dispatch("kanban.cmd.create", &payload, &mut state));
    make_writable(dir.path());

    assert_eq!(
        resp["code"], "STORE_NOT_DURABLE",
        "an unpersistable write must be reported as an error, got: {resp}"
    );
    assert!(
        resp["error"].as_str().unwrap_or("").contains("LOST"),
        "the client must be told the write is lost: {resp}"
    );
}

/// ...and the server must then refuse further mutations rather than keep
/// accepting writes into a buffer a restart would discard.
#[cfg(unix)]
#[test]
fn test_server_degrades_and_refuses_subsequent_mutations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    make_unwritable(dir.path());
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "first failure",
        "item_type": "chore",
    }))
    .unwrap();
    dispatch("kanban.cmd.create", &payload, &mut state); // trips the gate

    let second = response_json(&dispatch("kanban.cmd.create", &payload, &mut state));
    make_writable(dir.path());

    assert!(state.health.is_degraded(), "gate should be degraded");
    assert_eq!(
        second["code"], "STORE_DEGRADED",
        "subsequent mutations must be refused up front, got: {second}"
    );
    assert!(
        second["error"]
            .as_str()
            .unwrap_or("")
            .contains("NOT applied"),
        "refusal must state the change was not applied: {second}"
    );
}

/// A refused mutation must not touch in-memory state — that is the whole point
/// of admitting before applying.
#[cfg(unix)]
#[test]
fn test_refused_mutation_does_not_mutate_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Degrade the gate without going through a handler.
    state
        .health
        .record_persist_failure("create", "No space left on device");
    let before = state.store.active_item_count();

    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "should never exist",
        "item_type": "chore",
    }))
    .unwrap();
    let resp = response_json(&dispatch("kanban.cmd.create", &payload, &mut state));

    assert_eq!(resp["code"], "STORE_DEGRADED");
    assert_eq!(
        state.store.active_item_count(),
        before,
        "a refused mutation must leave the store untouched"
    );
}

/// Reads must keep working while degraded — a degraded board is still worth
/// reading, and going dark would be its own outage.
#[test]
fn test_reads_still_work_while_degraded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let id = create_item(
        &mut state,
        serde_json::json!({ "title": "readable", "item_type": "chore" }),
    );
    state
        .health
        .record_persist_failure("create", "No space left on device");

    let item = show_item_json(&mut state, &id);
    assert_eq!(
        item["title"], "readable",
        "reads must survive degraded mode"
    );

    let list_payload = serde_json::to_vec(&serde_json::json!({})).unwrap();
    let listed = response_json(&dispatch("kanban.cmd.list", &list_payload, &mut state));
    assert_ne!(listed["code"], "STORE_DEGRADED", "list must not be refused");
}

/// Once the store accepts writes again, the gate recovers on its own — a
/// transient full disk must not wedge the fleet until someone restarts.
#[test]
fn test_gate_recovers_and_mutations_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Degrade with a zero throttle so the next admission re-probes immediately.
    state.health = nusy_kanban_server::health::HealthGate::with_probe(
        1024,
        std::time::Duration::from_millis(0),
    );
    state
        .health
        .record_persist_failure("create", "No space left on device");
    assert!(state.health.is_degraded());

    // The dir is writable, so the probe should succeed and the write land.
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": "after recovery",
        "item_type": "chore",
    }))
    .unwrap();
    let resp = response_json(&dispatch("kanban.cmd.create", &payload, &mut state));

    assert!(!state.health.is_degraded(), "gate should have recovered");
    assert_ne!(
        resp["code"], "STORE_DEGRADED",
        "mutation should be admitted"
    );
    assert_eq!(state.store.active_item_count(), 1, "the write should land");
}

/// A healthy server must be completely unaffected — no false refusals.
#[test]
fn test_healthy_server_is_unaffected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let id = create_item(
        &mut state,
        serde_json::json!({ "title": "normal", "item_type": "chore" }),
    );
    assert!(
        !state.health.is_degraded(),
        "healthy store must stay healthy"
    );
    let item = show_item_json(&mut state, &id);
    assert_eq!(item["title"], "normal");
}

// ─── CH-6058 / SG-6057: the id floor, through the real dispatch path ────────

/// The SG-6057 collision, reproduced end-to-end: a rolled-back proposal store
/// whose max is PROP-3452 while PROP-3453/3454/3455 were already handed out.
/// Without the floor, dispatch hands out PROP-3453 — an id already merged on
/// main, with a reviewer mid-review on it.
#[cfg(feature = "pr")]
#[test]
fn test_id_floor_prevents_reminting_a_merged_proposal_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // The store believes 2001 is the highest id it has ever issued...
    let first = response_json(&dispatch(
        "kanban.cmd.pr.create",
        &serde_json::to_vec(&serde_json::json!({
            "title": "survivor of the rollback",
            "source_branch": "b1",
            "base": "main",
        }))
        .unwrap(),
        &mut state,
    ));
    assert_eq!(first["id"], "PROP-2001");

    // ...but the durable floor knows 2005 was already handed out and merged.
    let store_dir = nusy_kanban::persist::data_dir(dir.path()).expect("data dir");
    nusy_graph_review::IdFloor {
        proposals: 2005,
        items: 0,
    }
    .save(&store_dir)
    .expect("save floor");

    let next = response_json(&dispatch(
        "kanban.cmd.pr.create",
        &serde_json::to_vec(&serde_json::json!({
            "title": "must not collide",
            "source_branch": "b2",
            "base": "main",
        }))
        .unwrap(),
        &mut state,
    ));

    assert_eq!(
        next["id"], "PROP-2006",
        "allocator re-minted an id the floor says is taken: {next}"
    );
}

/// Every allocation must advance the persisted floor, so the protection
/// survives the next rollback rather than depending on someone seeding it.
#[cfg(feature = "pr")]
#[test]
fn test_pr_create_persists_the_id_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let created = response_json(&dispatch(
        "kanban.cmd.pr.create",
        &serde_json::to_vec(&serde_json::json!({
            "title": "advances the floor",
            "source_branch": "b",
            "base": "main",
        }))
        .unwrap(),
        &mut state,
    ));
    let id = created["id"].as_str().expect("id").to_string();
    let n: usize = id.strip_prefix("PROP-").unwrap().parse().unwrap();

    let store_dir = nusy_kanban::persist::data_dir(dir.path()).expect("data dir");
    let floor = nusy_graph_review::IdFloor::load(&store_dir);
    assert_eq!(
        floor.proposals, n,
        "the floor must record every id handed out, got {floor:?} for {id}"
    );
}

/// A healthy store must be unaffected — ids stay contiguous, no skipping.
#[cfg(feature = "pr")]
#[test]
fn test_id_floor_does_not_perturb_normal_allocation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let mut ids = Vec::new();
    for i in 0..3 {
        let resp = response_json(&dispatch(
            "kanban.cmd.pr.create",
            &serde_json::to_vec(&serde_json::json!({
                "title": format!("normal {i}"),
                "source_branch": format!("b{i}"),
                "base": "main",
            }))
            .unwrap(),
            &mut state,
        ));
        ids.push(resp["id"].as_str().expect("id").to_string());
    }
    assert_eq!(ids, vec!["PROP-2001", "PROP-2002", "PROP-2003"]);
}

// ─── CH-6109: typed relationships on create + edit ──────────────────────────
//
// Before this, `nk create` had NO relationship flag at all and `nk update` had only
// --related/--depends-on (flat, untyped, replace-semantics). An H→M→EXPR research trio
// was wired with --related, which discards direction and kind — you could not ask "which
// experiment validates this hypothesis?" without reading prose.

fn create_with(state: &mut ServerState, body: serde_json::Value) -> serde_json::Value {
    response_json(&dispatch(
        "kanban.cmd.create",
        &serde_json::to_vec(&body).unwrap(),
        state,
    ))
}

fn update_with(state: &mut ServerState, body: serde_json::Value) -> serde_json::Value {
    response_json(&dispatch(
        "kanban.cmd.update",
        &serde_json::to_vec(&body).unwrap(),
        state,
    ))
}

/// The headline: relationships can be set AT CREATE TIME, in one call.
#[test]
fn create_records_typed_relationships_in_one_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let voyage = create_with(
        &mut state,
        serde_json::json!({
            "title": "campaign", "item_type": "voyage",
        }),
    );
    let vy = voyage["id"].as_str().expect("voyage id").to_string();

    let exp = create_with(
        &mut state,
        serde_json::json!({
            "title": "feature", "item_type": "expedition",
            "relate": [format!("implements:{vy}")],
        }),
    );
    assert!(exp.get("error").is_none(), "create should succeed: {exp}");
    let rels = exp["relationships"]
        .as_array()
        .expect("relationships reported");
    assert_eq!(
        rels.len(),
        1,
        "the edge should be reported back, not assumed: {exp}"
    );
    assert_eq!(rels[0].as_str().unwrap(), format!("implements:{vy}"));
}

/// The research trio — the case --related could not express with direction or kind.
#[test]
fn the_h_m_expr_trio_is_expressible_at_create_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // Cross-board targets that this store has never seen: validation must infer their
    // type from the ID prefix and allow the edge, not refuse what it cannot look up.
    let expr = create_with(
        &mut state,
        serde_json::json!({
            "title": "battery", "item_type": "experiment",
            "relate": ["validates:H-5924"],
        }),
    );
    assert!(
        expr.get("error").is_none(),
        "cross-board edge must be allowed: {expr}"
    );

    let measure = create_with(
        &mut state,
        serde_json::json!({
            "title": "yield", "item_type": "measure",
            "relate": ["measures:EXPR-6073"],
        }),
    );
    assert!(measure.get("error").is_none(), "{measure}");

    let hyp = create_with(
        &mut state,
        serde_json::json!({
            "title": "claim", "item_type": "hypothesis",
            "relate": ["tests:PAPER-122"],
        }),
    );
    assert!(hyp.get("error").is_none(), "{hyp}");
}

/// related/dependsOn edges must ALSO land in the flat columns, or roadmap /
/// critical-path / worklist / nk show silently stop seeing relationships.
#[test]
fn related_and_depends_on_still_project_into_the_flat_columns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let a = create_with(
        &mut state,
        serde_json::json!({ "title": "upstream", "item_type": "chore" }),
    );
    let a_id = a["id"].as_str().unwrap().to_string();

    let b = create_with(
        &mut state,
        serde_json::json!({
            "title": "downstream", "item_type": "chore",
            "relate": [format!("dependsOn:{a_id}"), format!("related:{a_id}")],
        }),
    );
    let b_id = b["id"].as_str().unwrap().to_string();

    // Read it back the way every existing consumer does — the flat columns, not the
    // typed store. roadmap / critical-path / worklist all read these.
    let shown = show_item_json(&mut state, &b_id);
    let flat_related: Vec<String> = shown["related"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let flat_depends: Vec<String> = shown["depends_on"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    assert!(
        flat_related.contains(&a_id),
        "related edge must project into items.related (roadmap et al read this): {shown}"
    );
    assert!(
        flat_depends.contains(&a_id),
        "dependsOn edge must project into items.depends_on: {shown}"
    );

    // …and the SAME edges exist as typed rows. One write, two views — never two
    // independently-authored sources of truth.
    let typed: usize = state
        .relations
        .query_relations(&b_id)
        .iter()
        .map(|b| b.num_rows())
        .sum();
    assert_eq!(typed, 2, "both edges must also exist as typed rows");
}

/// Domain/range are ENFORCED — a typed vocabulary with unenforced domains is
/// documentation, not a schema.
#[test]
fn an_edge_violating_domain_or_range_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    // implements is Expedition -> Voyage; a chore cannot implement.
    let bad = create_with(
        &mut state,
        serde_json::json!({
            "title": "wrong domain", "item_type": "chore",
            "relate": ["implements:VY-1"],
        }),
    );
    assert_eq!(
        bad["code"], "INVALID_RELATION",
        "expected refusal, got: {bad}"
    );
    assert!(
        bad["error"].as_str().unwrap_or("").contains("chore"),
        "the error should name what was wrong: {bad}"
    );

    // An expedition cannot implement a chore (range).
    let bad2 = create_with(
        &mut state,
        serde_json::json!({
            "title": "wrong range", "item_type": "expedition",
            "relate": ["implements:CH-1"],
        }),
    );
    assert_eq!(bad2["code"], "INVALID_RELATION", "{bad2}");
}

/// A bad edge must refuse the WHOLE create — a half-related item is worse than none,
/// because the caller cannot tell which half landed.
#[test]
fn a_bad_edge_refuses_the_whole_create_leaving_no_item_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());
    let before = state.store.active_item_count();

    let resp = create_with(
        &mut state,
        serde_json::json!({
            "title": "should not exist", "item_type": "expedition",
            "relate": ["implements:VY-1", "frobnicates:CH-2"],   // second one is bogus
        }),
    );
    assert_eq!(resp["code"], "INVALID_RELATION");
    assert_eq!(
        state.store.active_item_count(),
        before,
        "no item may be created when any of its edges is invalid"
    );
}

#[test]
fn a_malformed_spec_is_refused_with_a_usable_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());
    let resp = create_with(
        &mut state,
        serde_json::json!({
            "title": "x", "item_type": "chore", "relate": ["noseparator"],
        }),
    );
    assert_eq!(resp["code"], "INVALID_RELATION");
    let msg = resp["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("predicate"),
        "message should show the expected form: {msg}"
    );
}

/// On EDIT, --relate is ADDITIVE: adding an edge never silently deletes the others.
/// That is the footgun --related/--depends-on still carry (they replace).
#[test]
fn update_relate_is_additive_not_replacing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let exp = create_with(
        &mut state,
        serde_json::json!({
            "title": "multi", "item_type": "expedition",
            "relate": ["implements:VY-100"],
        }),
    );
    let id = exp["id"].as_str().unwrap().to_string();

    let up = update_with(
        &mut state,
        serde_json::json!({
            "id": id, "relate": ["related:CH-200"],
        }),
    );
    assert!(up.get("error").is_none(), "{up}");
    let added = up["relationships_added"].as_array().expect("added list");
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].as_str().unwrap(), "related:CH-200");
    // The create-time edge must still be there — this is the whole point of additive.
    let edges = state.relations.query_relations(&id);
    let total: usize = edges.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total, 2,
        "the original implements edge must survive an added edge"
    );
}

#[test]
fn update_can_remove_an_edge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let exp = create_with(
        &mut state,
        serde_json::json!({
            "title": "removable", "item_type": "expedition",
            "relate": ["implements:VY-300"],
        }),
    );
    let id = exp["id"].as_str().unwrap().to_string();

    let up = update_with(
        &mut state,
        serde_json::json!({
            "id": id, "unrelate": ["implements:VY-300"],
        }),
    );
    assert!(up.get("error").is_none(), "{up}");
    assert_eq!(up["relationships_removed"].as_array().unwrap().len(), 1);
}

/// Creating with no --relate must behave exactly as before — no regression for every
/// existing caller.
#[test]
fn create_without_relate_is_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());
    let resp = create_with(
        &mut state,
        serde_json::json!({
            "title": "plain", "item_type": "chore",
        }),
    );
    assert!(resp.get("error").is_none(), "{resp}");
    assert!(resp["id"].as_str().is_some());
    assert!(
        resp.get("relationships").is_none()
            || resp["relationships"]
                .as_array()
                .is_none_or(|a| a.is_empty()),
        "no edges reported when none requested: {resp}"
    );
}

/// An item cannot relate to itself.
#[test]
fn a_self_edge_is_refused_on_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());
    let c = create_with(
        &mut state,
        serde_json::json!({ "title": "solo", "item_type": "chore" }),
    );
    let id = c["id"].as_str().unwrap().to_string();
    let up = update_with(
        &mut state,
        serde_json::json!({
            "id": id, "relate": [format!("related:{id}")],
        }),
    );
    assert_eq!(up["code"], "INVALID_RELATION", "{up}");
}

/// The invariant the module docs promise: the flat columns are a PROJECTION of the typed
/// edges, maintained in the same operation — on EDIT as well as create.
///
/// This regressed once already: create projected, update did not, so a `related` edge
/// added via update was invisible to roadmap / critical-path / worklist. The typed-store
/// assertions were all green while half the contract was broken — checking only the store
/// is not enough.
#[test]
fn update_keeps_the_flat_projection_in_step_not_just_the_typed_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());

    let up = create_with(
        &mut state,
        serde_json::json!({ "title": "upstream", "item_type": "chore" }),
    );
    let up_id = up["id"].as_str().unwrap().to_string();
    let it = create_with(
        &mut state,
        serde_json::json!({ "title": "item", "item_type": "chore" }),
    );
    let id = it["id"].as_str().unwrap().to_string();

    update_with(
        &mut state,
        serde_json::json!({
            "id": id, "relate": [format!("related:{up_id}"), format!("dependsOn:{up_id}")],
        }),
    );

    let shown = show_item_json(&mut state, &id);
    let flat_related: Vec<String> = shown["related"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let flat_depends: Vec<String> = shown["depends_on"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    assert!(
        flat_related.contains(&up_id),
        "update must project `related`: {shown}"
    );
    assert!(
        flat_depends.contains(&up_id),
        "update must project `dependsOn`: {shown}"
    );

    // …and removing the edge clears BOTH views, not just the typed one.
    update_with(
        &mut state,
        serde_json::json!({
            "id": id, "unrelate": [format!("related:{up_id}")],
        }),
    );
    let shown2 = show_item_json(&mut state, &id);
    let after: Vec<String> = shown2["related"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !after.contains(&up_id),
        "unrelate must clear the flat projection too: {shown2}"
    );
}

/// A predicate that does NOT project must leave the flat columns alone — otherwise every
/// typed edge would leak into `related` and the distinction would be meaningless.
#[test]
fn a_non_projecting_predicate_does_not_touch_the_flat_columns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = test_state(dir.path());
    let exp = create_with(
        &mut state,
        serde_json::json!({
            "title": "e", "item_type": "expedition", "relate": ["implements:VY-9000"],
        }),
    );
    let id = exp["id"].as_str().unwrap().to_string();
    let shown = show_item_json(&mut state, &id);
    let flat: Vec<String> = shown["related"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        flat.is_empty(),
        "`implements` must not leak into related: {shown}"
    );
}
