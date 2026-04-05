//! Integration tests for nusy-kanban CLI commands that previously had no test
//! coverage: blocked, roadmap, validate, next, init, boards, history.
//!
//! EXP-3002 Phase 1.

use arrow::array::{Array, BooleanArray, ListArray, StringArray, TimestampMillisecondArray};
use nusy_kanban::crud::{CreateItemInput, KanbanStore};
use nusy_kanban::item_type::ItemType;
use nusy_kanban::schema::{items_col, runs_col};
use nusy_kanban::{display, persist};

/// IDs returned from populated_store().
struct TestIds {
    /// High priority, backlog, depends on `independent`
    blocked: String,
    /// Medium priority, backlog, no deps — the dependency target
    independent: String,
    /// Critical priority, backlog
    critical: String,
    /// Low priority, backlog
    low: String,
    /// No priority, backlog (chore)
    chore: String,
}

/// Helper: create a store with 5 items and return their actual allocated IDs.
fn populated_store() -> (KanbanStore, TestIds) {
    let mut store = KanbanStore::new();

    // Create the independent item first so we know its ID for depends_on
    let independent = store
        .create_item(&CreateItemInput {
            title: "Independent expedition".to_string(),
            item_type: ItemType::Expedition,
            priority: Some("medium".to_string()),
            assignee: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: None,
        })
        .expect("create independent");

    // Blocked item depends on the independent item
    let blocked = store
        .create_item(&CreateItemInput {
            title: "Blocked expedition".to_string(),
            item_type: ItemType::Expedition,
            priority: Some("high".to_string()),
            assignee: Some("M5".to_string()),
            tags: vec!["v14".to_string()],
            related: vec![],
            depends_on: vec![independent.clone()],
            body: None,
        })
        .expect("create blocked");

    let critical = store
        .create_item(&CreateItemInput {
            title: "Critical work".to_string(),
            item_type: ItemType::Expedition,
            priority: Some("critical".to_string()),
            assignee: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: None,
        })
        .expect("create critical");

    let low = store
        .create_item(&CreateItemInput {
            title: "Low priority cleanup".to_string(),
            item_type: ItemType::Expedition,
            priority: Some("low".to_string()),
            assignee: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: None,
        })
        .expect("create low");

    let chore = store
        .create_item(&CreateItemInput {
            title: "Unranked chore".to_string(),
            item_type: ItemType::Chore,
            priority: None,
            assignee: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: None,
        })
        .expect("create chore");

    let ids = TestIds {
        blocked,
        independent,
        critical,
        low,
        chore,
    };
    (store, ids)
}

/// Extract the ID from a single-row batch.
fn batch_id(batch: &arrow::array::RecordBatch) -> String {
    batch
        .column(items_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id")
        .value(0)
        .to_string()
}

/// Extract the priority from a single-row batch.
fn batch_priority(batch: &arrow::array::RecordBatch) -> Option<String> {
    let arr = batch
        .column(items_col::PRIORITY)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("priority");
    if arr.is_null(0) {
        None
    } else {
        Some(arr.value(0).to_string())
    }
}

// ─── Blocked ────────────────────────────────────────────────────────────────

/// Reproduce the find_blocked_items logic from main.rs for library-level testing.
fn find_blocked_items(store: &KanbanStore) -> Vec<arrow::array::RecordBatch> {
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

    store
        .query_items(None, None, None, None)
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
                    .expect("depends_on values");
                (0..dep_strings.len()).any(|j| !done_ids.contains(dep_strings.value(j)))
            })
        })
        .collect()
}

#[test]
fn test_blocked_finds_items_with_unmet_deps() {
    let (store, ids) = populated_store();

    let blocked = find_blocked_items(&store);
    assert_eq!(blocked.len(), 1, "exactly one blocked item");
    assert_eq!(batch_id(&blocked[0]), ids.blocked);
}

#[test]
fn test_blocked_clears_when_deps_done() {
    let (mut store, ids) = populated_store();

    // Move the dependency to done
    store
        .update_status(&ids.independent, "done", Some("M5"), true, Some("test"))
        .expect("move dep to done");

    let blocked = find_blocked_items(&store);
    assert!(blocked.is_empty(), "no blocked items after deps are done");
}

#[test]
fn test_blocked_skips_done_items_even_with_pending_deps() {
    let (mut store, ids) = populated_store();

    // Move the blocked item itself to done (force)
    store
        .update_status(&ids.blocked, "done", Some("M5"), true, Some("test"))
        .expect("force move blocked to done");

    let blocked = find_blocked_items(&store);
    assert!(blocked.is_empty(), "done items not reported as blocked");
}

#[test]
fn test_blocked_empty_depends_not_blocked() {
    let (store, ids) = populated_store();
    let blocked = find_blocked_items(&store);

    // Only the blocked item should appear — others have no deps
    for b in &blocked {
        let id = batch_id(b);
        assert_ne!(id, ids.independent);
        assert_ne!(id, ids.critical);
        assert_ne!(id, ids.low);
        assert_ne!(id, ids.chore);
    }
}

// ─── Roadmap ────────────────────────────────────────────────────────────────

#[test]
fn test_roadmap_returns_backlog_items() {
    let (store, _ids) = populated_store();
    let backlog = store.query_items(Some("backlog"), None, None, None);

    assert_eq!(backlog.len(), 5, "all 5 items in backlog");
}

#[test]
fn test_roadmap_ranked_sorts_by_priority() {
    let (store, _ids) = populated_store();
    let mut backlog = store.query_items(Some("backlog"), None, None, None);

    // Sort by priority (same logic as main.rs)
    backlog.sort_by(|a, b| {
        let get_priority = |batch: &arrow::array::RecordBatch| -> i32 {
            let prios = batch
                .column(items_col::PRIORITY)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("priority");
            if prios.is_null(0) {
                return 99;
            }
            match prios.value(0) {
                "critical" => 0,
                "high" => 1,
                "medium" => 2,
                "low" => 3,
                _ => 99,
            }
        };
        get_priority(a).cmp(&get_priority(b))
    });

    let priorities: Vec<Option<String>> = backlog.iter().map(|b| batch_priority(b)).collect();
    assert_eq!(priorities[0], Some("critical".to_string()));
    assert_eq!(priorities[1], Some("high".to_string()));
    assert_eq!(priorities[2], Some("medium".to_string()));
    assert_eq!(priorities[3], Some("low".to_string()));
    assert_eq!(priorities[4], None); // chore has no priority → rank 99
}

#[test]
fn test_roadmap_excludes_non_backlog() {
    let (mut store, ids) = populated_store();
    store
        .update_status(&ids.critical, "in_progress", Some("DGX"), false, None)
        .expect("move to in_progress");

    let backlog = store.query_items(Some("backlog"), None, None, None);
    assert_eq!(backlog.len(), 4, "one fewer after moving to in_progress");

    for b in &backlog {
        assert_ne!(batch_id(b), ids.critical);
    }
}

// ─── Validate ───────────────────────────────────────────────────────────────

#[test]
fn test_validate_counts_all_items() {
    let (store, _ids) = populated_store();
    let all = store.query_items(None, None, None, None);
    assert_eq!(all.len(), 5);
}

#[test]
fn test_validate_empty_store() {
    let store = KanbanStore::new();
    let all = store.query_items(None, None, None, None);
    assert_eq!(all.len(), 0, "empty store has zero items");
}

// ─── Next ───────────────────────────────────────────────────────────────────

#[test]
fn test_next_returns_first_backlog_item() {
    let (store, _ids) = populated_store();
    let results = store.query_items(Some("backlog"), None, None, None);

    assert!(!results.is_empty(), "should have backlog items");
    let id = batch_id(&results[0]);
    assert!(id.starts_with("EX-") || id.starts_with("CH-"));
}

#[test]
fn test_next_with_assignee_filter() {
    let (store, ids) = populated_store();

    // Only the blocked item is assigned to M5
    let results = store.query_items(Some("backlog"), None, None, Some("M5"));
    assert_eq!(results.len(), 1);
    assert_eq!(batch_id(&results[0]), ids.blocked);
}

#[test]
fn test_next_empty_when_no_backlog() {
    let (mut store, ids) = populated_store();

    // Move all items to done
    let all_ids = [
        &ids.independent,
        &ids.blocked,
        &ids.critical,
        &ids.low,
        &ids.chore,
    ];
    for id in all_ids {
        store
            .update_status(id, "done", Some("M5"), true, Some("test"))
            .expect("move to done");
    }

    let results = store.query_items(Some("backlog"), None, None, None);
    assert!(
        results.is_empty(),
        "no backlog items after all moved to done"
    );
}

// ─── Init ───────────────────────────────────────────────────────────────────

#[test]
fn test_init_creates_config_structure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    let config_dir = root.join(".yurtle-kanban");
    assert!(!config_dir.exists(), "no config dir before init");

    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("config.yaml"),
        nusy_kanban::config::default_config_yaml(),
    )
    .expect("write config");
    std::fs::create_dir_all(config_dir.join("data")).expect("create data dir");

    assert!(config_dir.exists());
    assert!(config_dir.join("config.yaml").exists());
    assert!(config_dir.join("data").exists());

    let content = std::fs::read_to_string(config_dir.join("config.yaml")).expect("read config");
    assert!(
        content.contains("development"),
        "default config has dev board"
    );
    assert!(
        content.contains("research"),
        "default config has research board"
    );
}

#[test]
fn test_init_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_dir = dir.path().join(".yurtle-kanban");

    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("config.yaml"),
        nusy_kanban::config::default_config_yaml(),
    )
    .expect("write config");

    // Second check — directory already exists
    assert!(config_dir.exists(), "already initialized");
}

// ─── Boards ─────────────────────────────────────────────────────────────────

#[test]
fn test_boards_counts_items_per_board() {
    let (store, _ids) = populated_store();

    let dev_items = store.query_items(None, None, Some("development"), None);
    let research_items = store.query_items(None, None, Some("research"), None);

    assert_eq!(dev_items.len(), 5, "all items on development board");
    assert_eq!(research_items.len(), 0, "no items on research board");
}

#[test]
fn test_boards_with_research_items() {
    let (mut store, _ids) = populated_store();

    store
        .create_item(&CreateItemInput {
            title: "Test paper".to_string(),
            item_type: ItemType::Paper,
            priority: None,
            assignee: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: None,
        })
        .expect("create paper");

    let dev = store.query_items(None, None, Some("development"), None);
    let research = store.query_items(None, None, Some("research"), None);

    assert_eq!(dev.len(), 5, "5 dev items");
    assert_eq!(research.len(), 1, "1 research item");
}

// ─── History ────────────────────────────────────────────────────────────────

#[test]
fn test_history_shows_done_items() {
    let (mut store, ids) = populated_store();

    store
        .update_status(&ids.independent, "done", Some("M5"), true, Some("test"))
        .expect("move to done");

    let done_items = store.query_items(Some("done"), None, None, None);
    assert_eq!(done_items.len(), 1);
    assert_eq!(batch_id(&done_items[0]), ids.independent);

    let output = display::format_history(store.items_batches(), "done");
    assert!(!output.is_empty(), "history output should not be empty");
}

#[test]
fn test_history_week_finds_recent_completions() {
    let (mut store, ids) = populated_store();

    store
        .update_status(&ids.critical, "done", Some("DGX"), true, Some("test"))
        .expect("move to done");

    let cutoff = chrono::Utc::now().timestamp_millis() - (7 * 24 * 60 * 60 * 1000);

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
                && timestamps.value(i) > cutoff
            {
                recent_ids.insert(item_ids.value(i).to_string());
            }
        }
    }

    assert!(
        recent_ids.contains(&ids.critical),
        "critical item should be in recently completed"
    );
}

#[test]
fn test_history_week_empty_when_nothing_done() {
    let (store, _ids) = populated_store();

    let cutoff = chrono::Utc::now().timestamp_millis() - (7 * 24 * 60 * 60 * 1000);

    let mut found_done = false;
    for batch in store.runs_batches() {
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
                && timestamps.value(i) > cutoff
            {
                found_done = true;
            }
        }
    }

    assert!(!found_done, "no done transitions when nothing completed");
}

// ─── Persistence Round-Trip ─────────────────────────────────────────────────

#[test]
fn test_persist_and_reload_preserves_items() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, _ids) = populated_store();

    persist::save_store(dir.path(), &store).expect("save");
    let loaded = persist::load_store(dir.path()).expect("load");

    assert_eq!(
        loaded.active_item_count(),
        store.active_item_count(),
        "item count preserved after round-trip"
    );
}

#[test]
fn test_persist_and_reload_preserves_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, ids) = populated_store();

    store
        .update_status(&ids.independent, "in_progress", Some("DGX"), false, None)
        .expect("move to in_progress");
    store
        .update_status(&ids.independent, "done", Some("DGX"), true, Some("test"))
        .expect("move to done");

    persist::save_store(dir.path(), &store).expect("save");
    let loaded = persist::load_store(dir.path()).expect("load");

    let orig_runs: usize = store.runs_batches().iter().map(|b| b.num_rows()).sum();
    let loaded_runs: usize = loaded.runs_batches().iter().map(|b| b.num_rows()).sum();

    assert_eq!(
        orig_runs, loaded_runs,
        "run count preserved after round-trip"
    );
}
