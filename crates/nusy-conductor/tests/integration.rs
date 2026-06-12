//! Integration tests for nusy-conductor.
//!
//! These tests exercise the public API surface with realistic data patterns
//! matching actual kanban items. They verify cross-module interactions
//! (reader → decomposer → state) without requiring a live NATS server.

use nusy_conductor::decomposer::{
    PhaseExtractor, StructuralExtractor, SuggestedAction, analyze_expedition,
};
use nusy_conductor::monitor::{BlockerMonitor, format_daily_summary};
use nusy_conductor::reader::{WorkGraph, WorkItem};
use nusy_conductor::review_cycle::{ConductorAction, ReviewCycleEngine};
use nusy_conductor::state::AssigneeTracker;

/// Build a realistic work graph matching the actual NuSy board.
fn realistic_graph() -> WorkGraph {
    WorkGraph::from_items(vec![
        WorkItem {
            id: "VY-3045".to_string(),
            title: "V8v1: Conductor v1 — Lightweight Orchestration".to_string(),
            item_type: "voyage".to_string(),
            status: "in_progress".to_string(),
            priority: Some("high".to_string()),
            assignee: None,
            board: Some("development".to_string()),
            tags: vec!["v14".to_string()],
            related: vec![],
            depends_on: vec![],
            body: Some("Voyage for conductor orchestration".to_string()),
        },
        WorkItem {
            id: "EX-3047".to_string(),
            title: "V8v1: Conductor Foundation — Kanban Reader + State Engine".to_string(),
            item_type: "expedition".to_string(),
            status: "in_progress".to_string(),
            priority: Some("high".to_string()),
            assignee: Some("M5".to_string()),
            board: Some("development".to_string()),
            tags: vec!["v14".to_string(), "rust".to_string()],
            related: vec!["VY-3045".to_string()],
            depends_on: vec![],
            body: Some(REAL_EXPEDITION_BODY.to_string()),
        },
        WorkItem {
            id: "EX-3048".to_string(),
            title: "V8v1: Review Cycle Automation".to_string(),
            item_type: "expedition".to_string(),
            status: "backlog".to_string(),
            priority: Some("high".to_string()),
            assignee: Some("M5".to_string()),
            board: Some("development".to_string()),
            tags: vec!["v14".to_string()],
            related: vec!["VY-3045".to_string()],
            depends_on: vec!["EX-3047".to_string()],
            body: None,
        },
        WorkItem {
            id: "EX-3049".to_string(),
            title: "V8v1: Blocker Detection + Escalation".to_string(),
            item_type: "expedition".to_string(),
            status: "backlog".to_string(),
            priority: Some("medium".to_string()),
            assignee: Some("Mini".to_string()),
            board: Some("development".to_string()),
            tags: vec!["v14".to_string()],
            related: vec!["VY-3045".to_string()],
            depends_on: vec!["EX-3047".to_string()],
            body: None,
        },
        WorkItem {
            id: "EX-3040".to_string(),
            title: "P133-E3: Arrow-Native FK Store".to_string(),
            item_type: "expedition".to_string(),
            status: "backlog".to_string(),
            priority: Some("high".to_string()),
            assignee: None,
            board: Some("development".to_string()),
            tags: vec!["v14".to_string(), "arrow".to_string()],
            related: vec![],
            depends_on: vec![],
            body: None,
        },
        WorkItem {
            id: "CH-3050".to_string(),
            title: "GPU cluster health check".to_string(),
            item_type: "chore".to_string(),
            status: "backlog".to_string(),
            priority: Some("low".to_string()),
            assignee: None,
            board: Some("development".to_string()),
            tags: vec!["gpu".to_string()],
            related: vec![],
            depends_on: vec![],
            body: None,
        },
    ])
}

const REAL_EXPEDITION_BODY: &str = r#"## Phase 1: Kanban State Reader

- Query Arrow kanban via NATS for all items by status
- Parse item metadata: type, status, assignee, priority, relations
- Build in-memory work graph: items + dependencies + assignments
- Subscribe to kanban.event.* for real-time state updates
- **Done when:** Reader produces correct status summary for 20+ items

## Phase 2: Expedition Decomposer

- Read expedition body content to extract phases
- Use LLM judgment for natural language phase extraction
- Determine current phase
- Suggest next action for each in-progress item
- **Done when:** Expedition with 5 phases correctly identifies current phase and suggests next

## Phase 3: Assignee Tracker

- Track which agent is working on what
- Track agent availability: agent with 0 in-progress items = available
- Track agent capabilities: DGX = GPU work, M5 = architecture, Mini = infrastructure
- **Done when:** Tracker correctly identifies available agents and suggests assignment"#;

// --- Cross-module integration tests ---

#[test]
fn test_full_pipeline_reader_decomposer_state() {
    // Simulate: read graph → decompose expedition → suggest assignment
    let graph = realistic_graph();

    // Step 1: Reader built the graph correctly
    assert_eq!(graph.items.len(), 6);
    let summary = graph.status_summary();
    assert_eq!(summary.get("in_progress"), Some(&2));
    assert_eq!(summary.get("backlog"), Some(&4));

    // Step 2: Decompose the in-progress expedition
    let expedition = graph.items.get("EX-3047").expect("expedition exists");
    let progress = analyze_expedition(expedition, &[]);

    assert_eq!(progress.phases.len(), 3);
    // No evidence → should suggest starting phase 1
    match &progress.suggested_action {
        SuggestedAction::StartPhase {
            phase_number,
            title,
        } => {
            assert_eq!(*phase_number, 1);
            assert_eq!(title, "Kanban State Reader");
        }
        other => panic!("expected StartPhase, got {other:?}"),
    }

    // Step 3: Check assignment for unassigned backlog item
    let tracker = AssigneeTracker::with_defaults();
    let suggestion = tracker.suggest_assignment(&graph, "EX-3040");
    // M5 is busy (in_progress on EX-3047), Mini has EX-3049 in backlog
    // DGX has no assigned items — should be suggested
    assert!(suggestion.suggested_agent.is_some());
}

#[test]
fn test_dependency_chain_blocks_downstream() {
    let graph = realistic_graph();

    // EX-3048 and EX-3049 depend on EX-3047 which is in_progress
    let blocked = graph.blocked_items();
    let blocked_ids: Vec<&str> = blocked.iter().map(|i| i.id.as_str()).collect();

    assert!(blocked_ids.contains(&"EX-3048"));
    assert!(blocked_ids.contains(&"EX-3049"));
    // EX-3047 itself is not blocked (no dependencies)
    assert!(!blocked_ids.contains(&"EX-3047"));
}

#[test]
fn test_event_stream_updates_graph_and_assignment() {
    let mut graph = realistic_graph();

    // Simulate: EX-3047 moves to done
    graph.apply_moved("EX-3047", "done");

    // EX-3048 should no longer be blocked
    let blocked = graph.blocked_items();
    let blocked_ids: Vec<&str> = blocked.iter().map(|i| i.id.as_str()).collect();
    assert!(!blocked_ids.contains(&"EX-3048"));
    assert!(!blocked_ids.contains(&"EX-3049"));

    // M5 is now available (EX-3047 is done, not in_progress)
    let tracker = AssigneeTracker::with_defaults();
    let available = tracker.available_agents(&graph);
    let available_names: Vec<&str> = available.iter().map(|a| a.name.as_str()).collect();
    assert!(available_names.contains(&"M5"));
}

#[test]
fn test_gpu_assignment_prefers_dgx() {
    let graph = realistic_graph();
    let tracker = AssigneeTracker::with_defaults();

    // CH-3050 is tagged "gpu" — should go to DGX
    let suggestion = tracker.suggest_assignment(&graph, "CH-3050");
    assert_eq!(suggestion.suggested_agent, Some("DGX".to_string()));
}

#[test]
fn test_structural_extractor_matches_real_format() {
    let extractor = StructuralExtractor;
    let phases = extractor.extract_phases(REAL_EXPEDITION_BODY);

    assert_eq!(phases.len(), 3);

    // Phase 1 details
    assert_eq!(phases[0].number, 1);
    assert_eq!(phases[0].title, "Kanban State Reader");
    assert_eq!(phases[0].tasks.len(), 4);
    assert!(phases[0].done_criteria.is_some());
    let criteria = phases[0].done_criteria.as_ref().unwrap();
    assert!(criteria.contains("status summary"));

    // Phase 2 details
    assert_eq!(phases[1].number, 2);
    assert_eq!(phases[1].title, "Expedition Decomposer");
    assert_eq!(phases[1].tasks.len(), 4);

    // Phase 3 details
    assert_eq!(phases[2].number, 3);
    assert_eq!(phases[2].title, "Assignee Tracker");
    assert_eq!(phases[2].tasks.len(), 3);
}

#[test]
fn test_expedition_progress_with_evidence() {
    let item = WorkItem {
        id: "EX-3047".to_string(),
        title: "Conductor Foundation".to_string(),
        item_type: "expedition".to_string(),
        status: "in_progress".to_string(),
        priority: Some("high".to_string()),
        assignee: Some("M5".to_string()),
        board: Some("development".to_string()),
        tags: vec![],
        related: vec![],
        depends_on: vec![],
        body: Some(REAL_EXPEDITION_BODY.to_string()),
    };

    // Evidence that phase 1 is complete and phase 2 is in progress
    let evidence = &[
        "reader produces correct status summary for 20+ items, NATS subscription working",
        "expedition decomposer extracts phases from body content, working on current phase detection",
    ];

    let progress = analyze_expedition(&item, evidence);
    assert_eq!(progress.phases.len(), 3);
    // Should recognize work on phase 2 (decomposer)
    assert!(progress.current_phase.is_some());
}

#[test]
fn test_work_graph_create_delete_cycle() {
    let mut graph = realistic_graph();
    let initial_count = graph.items.len();

    // Create a new item
    graph.apply_created(WorkItem {
        id: "EX-3060".to_string(),
        title: "New expedition".to_string(),
        item_type: "expedition".to_string(),
        status: "backlog".to_string(),
        priority: None,
        assignee: None,
        board: Some("development".to_string()),
        tags: vec![],
        related: vec![],
        depends_on: vec!["EX-3047".to_string()],
        body: None,
    });
    assert_eq!(graph.items.len(), initial_count + 1);

    // Delete it
    graph.apply_deleted("EX-3060");
    assert_eq!(graph.items.len(), initial_count);
    // Dependency edges cleaned up
    assert!(!graph.depends_on.contains_key("EX-3060"));
}

#[test]
fn test_infer_capabilities_from_realistic_items() {
    let graph = realistic_graph();
    let tracker = AssigneeTracker::with_defaults();

    // EX-3040 has "arrow" tag → should infer Rust capability
    let suggestion = tracker.suggest_assignment(&graph, "EX-3040");
    assert!(
        suggestion.suggested_agent.is_some(),
        "should suggest an agent for Rust/Arrow work"
    );
}

// --- Review cycle integration tests ---

fn make_simple_item(id: &str, status: &str, assignee: Option<&str>) -> WorkItem {
    WorkItem {
        id: id.to_string(),
        title: "Test".to_string(),
        item_type: "expedition".to_string(),
        status: status.to_string(),
        priority: None,
        assignee: assignee.map(String::from),
        board: Some("development".to_string()),
        tags: vec![],
        related: vec![],
        depends_on: vec![],
        body: None,
    }
}

#[test]
fn test_review_cycle_end_to_end_chore_auto_approve() {
    // Full pipeline: detect chore PR → assign reviewer → approved → auto-merge
    let mut engine = ReviewCycleEngine::new();
    let graph = WorkGraph::from_items(vec![make_simple_item("EX-100", "in_progress", Some("M5"))]);

    // Step 1: Detect a chore PR from Mini → triggers CI
    let actions = engine.on_proposal_detected(
        "PROP-3000",
        "CH-3020: Add resolve command",
        "expedition/ch-3020-add-resolve",
        "Mini",
        Some("CH-3020"),
        Some("chore"),
        &graph,
    );

    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ConductorAction::TriggerCi { .. }))
    );

    // Step 1b: CI passes → assigns reviewer
    let actions = engine.on_ci_result("PROP-3000", true, "all passed", &graph);
    let assign = actions
        .iter()
        .find(|a| matches!(a, ConductorAction::AssignReviewer { .. }));
    assert!(assign.is_some());
    if let Some(ConductorAction::AssignReviewer { reviewer, .. }) = assign {
        assert_ne!(reviewer, "Mini");
    }

    // Step 2: Reviewer approves
    let actions = engine.on_review_result("PROP-3000", true);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ConductorAction::AutoApprove { .. }))
    );
}

#[test]
fn test_review_cycle_expedition_requires_captain() {
    let mut engine = ReviewCycleEngine::new();
    let graph = WorkGraph::from_items(vec![]);

    engine.on_proposal_detected(
        "PROP-3001",
        "EX-3047: Conductor Foundation",
        "expedition/ex-3047-conductor",
        "M5",
        Some("EX-3047"),
        Some("expedition"),
        &graph,
    );

    // CI passes → assigns reviewer
    engine.on_ci_result("PROP-3001", true, "all passed", &graph);

    let actions = engine.on_review_result("PROP-3001", true);
    // Expedition → Captain required even after reviewer approval
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ConductorAction::EscalateToCaptain { .. }))
    );
}

#[test]
fn test_review_cycle_rejection_fix_approve() {
    let mut engine = ReviewCycleEngine::new();
    let graph = WorkGraph::from_items(vec![]);

    // Detect → CI → reviewer
    engine.on_proposal_detected(
        "PROP-3002",
        "CH-3020: Fix",
        "expedition/ch-3020-fix",
        "DGX",
        Some("CH-3020"),
        Some("chore"),
        &graph,
    );
    engine.on_ci_result("PROP-3002", true, "all passed", &graph);

    // Round 1: rejected
    let actions = engine.on_review_result("PROP-3002", false);
    assert!(actions.iter().any(|a| matches!(
        a,
        ConductorAction::RouteToImplementer { implementer, .. }
        if implementer == "DGX"
    )));

    // Fix pushed
    engine.on_fixes_pushed("PROP-3002");

    // Round 2: approved → auto-approve (chore)
    let actions = engine.on_review_result("PROP-3002", true);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ConductorAction::AutoApprove { .. }))
    );
}

#[test]
fn test_review_cycle_with_decomposer_and_state() {
    // Full cross-module: decompose expedition → track state → route review
    let mut engine = ReviewCycleEngine::new();

    let body = r#"## Phase 1: Setup
- Install deps
- **Done when:** Deps installed

## Phase 2: Implement
- Write code
- **Done when:** Tests pass"#;

    let item = WorkItem {
        id: "EX-3048".to_string(),
        title: "Review Cycle Automation".to_string(),
        item_type: "expedition".to_string(),
        status: "in_progress".to_string(),
        priority: Some("high".to_string()),
        assignee: Some("Mini".to_string()),
        board: Some("development".to_string()),
        tags: vec![],
        related: vec![],
        depends_on: vec![],
        body: Some(body.to_string()),
    };

    let graph = WorkGraph::from_items(vec![item.clone()]);

    // Decompose the expedition
    let progress = analyze_expedition(&item, &["setup complete, deps installed"]);
    assert_eq!(progress.phases.len(), 2);

    // Track agent state
    let tracker = AssigneeTracker::with_defaults();
    let available = tracker.available_agents(&graph);
    // Mini is busy (in_progress), M5 and DGX are free
    let available_names: Vec<&str> = available.iter().map(|a| a.name.as_str()).collect();
    assert!(!available_names.contains(&"Mini"));

    // Detect proposal → CI → assign reviewer (not Mini)
    let actions = engine.on_proposal_detected(
        "PROP-3003",
        "EX-3048: Review Cycle",
        "expedition/ex-3048-review-cycle",
        "Mini",
        Some("EX-3048"),
        Some("expedition"),
        &graph,
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ConductorAction::TriggerCi { .. }))
    );

    let actions = engine.on_ci_result("PROP-3003", true, "all passed", &graph);
    let assign = actions
        .iter()
        .find(|a| matches!(a, ConductorAction::AssignReviewer { .. }));
    if let Some(ConductorAction::AssignReviewer { reviewer, .. }) = assign {
        assert_ne!(reviewer, "Mini");
    }
}

// --- Monitor integration tests ---

#[test]
fn test_monitor_with_realistic_graph() {
    let monitor = BlockerMonitor::new();
    let graph = realistic_graph();

    let summary = monitor.daily_summary(&graph);
    // Realistic graph has items in multiple states
    assert!(!summary.status_counts.is_empty());
    assert!(summary.agent_states.len() == 3); // M5, DGX, Mini

    // Format should produce valid markdown
    let formatted = format_daily_summary(&summary);
    assert!(formatted.contains("# Daily Board Summary"));
}

#[test]
fn test_monitor_blocked_by_dependency_chain() {
    // EX-3048 depends on EX-3047 (in_progress) → should be flagged
    let graph = realistic_graph();
    let monitor = BlockerMonitor::new();

    let blocked = monitor.detect_blocked_items(&graph);
    let blocked_ids: Vec<&str> = blocked.iter().map(|f| f.id.as_str()).collect();

    // EX-3048 and EX-3049 both depend on EX-3047
    assert!(blocked_ids.contains(&"EX-3048"));
    assert!(blocked_ids.contains(&"EX-3049"));
}

#[test]
fn test_monitor_event_updates_clear_blockers() {
    let mut graph = realistic_graph();
    let monitor = BlockerMonitor::new();

    // Before: EX-3048 blocked by EX-3047
    let blocked_before = monitor.detect_blocked_items(&graph);
    assert!(blocked_before.iter().any(|f| f.id == "EX-3048"));

    // Event: EX-3047 moves to done
    graph.apply_moved("EX-3047", "done");

    // After: EX-3048 no longer blocked
    let blocked_after = monitor.detect_blocked_items(&graph);
    assert!(!blocked_after.iter().any(|f| f.id == "EX-3048"));
}
