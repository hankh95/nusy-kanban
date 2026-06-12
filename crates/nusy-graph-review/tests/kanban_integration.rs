//! EXP-1288 Phase 4: End-to-end integration tests for kanban ↔ proposal bridge.
//!
//! Tests the full flow: proposal → kanban board → experiment → research board
//! → hypothesis evidence → cross-board links.

use nusy_graph_review::ProposalStatus;
use nusy_graph_review::experiment_bridge::{
    ExperimentOutcome, MetricComparison, generate_experiment_record, render_experiment_markdown,
};
use nusy_graph_review::hdd_loop::{accumulate_evidence, generate_cross_board_links};
use nusy_graph_review::kanban_bridge::{
    KanbanAction, on_proposal_created, on_proposal_transition, safety_to_priority,
};
use nusy_graph_review::safety_gates::{classify_change, default_gates};

/// Test scenario 1: Approved proposal (happy path)
///
/// Being creates Y0 knowledge change → auto-approvable → shadow passes →
/// merges → EXPR created with VALIDATED → cross-board links generated.
#[test]
fn test_scenario_1_approved_proposal_happy_path() {
    let gates = default_gates().unwrap();

    // 1. Being creates a Y0 knowledge change proposal
    let req = classify_change(&gates, 0, "general");
    assert!(!req.requires_human, "Y0 general should be auto-approvable");

    // 2. Verify kanban action creates item on dev board
    let action = on_proposal_created(
        "prop-001",
        "Add botany triples",
        "santiago",
        "knowledge_change",
        &req,
    );
    match &action {
        KanbanAction::CreateItem { priority, tags, .. } => {
            assert_eq!(priority, "low"); // Auto-approvable = low priority
            assert!(tags.contains(&"self-modification".to_string()));
            assert!(!tags.contains(&"human-gate".to_string()));
        }
        _ => panic!("Expected CreateItem"),
    }

    // 3-5. Shadow eval passes, Y6 approves (simulate)
    // 6. Proposal merges
    let merge_action = on_proposal_transition("prop-001", &ProposalStatus::Merged);
    match &merge_action {
        KanbanAction::Complete { outcome, .. } => assert_eq!(outcome, "merged"),
        _ => panic!("Expected Complete"),
    }

    // 7-8. EXPR created on research board
    let record = generate_experiment_record(
        "prop-001",
        "Add botany triples",
        "santiago-developer-v14.3",
        "adding botanical entity triples to Y0",
        0,
        "general",
        "Added 15 Y0 prose chunks about photosynthesis",
        "Y0 general: auto-approve",
        vec![MetricComparison {
            name: "cq_coverage".to_string(),
            baseline: 0.82,
            shadow: 0.85,
            delta: 0.03,
        }],
        ExperimentOutcome::Validated,
        "CQ coverage improved 3% with no regressions",
        1,
    );
    assert_eq!(record.outcome, ExperimentOutcome::Validated);
    assert_eq!(record.experiment_id, "EXPR-auto-001");

    // Verify markdown renders correctly
    let md = render_experiment_markdown(&record);
    assert!(md.contains("**VALIDATED**"));
    assert!(md.contains("PROP-prop-001"));

    // 9. Cross-board links
    let links = generate_cross_board_links("prop-001", "EXPR-auto-001", Some("H-014"));
    assert_eq!(links.len(), 3);
    assert_eq!(links[0].predicate, "kb:producedExperiment");
    assert_eq!(links[1].predicate, "kb:testedHypothesis");
    assert_eq!(links[2].predicate, "kb:evidencedBy");
}

/// Test scenario 2: Rejected proposal
///
/// Being creates Y2 reasoning change → human-gated → shadow shows regression
/// → rejected → EXPR with REFUTED.
#[test]
fn test_scenario_2_rejected_proposal() {
    let gates = default_gates().unwrap();

    // 1. Being creates Y2 reasoning rule change
    let req = classify_change(&gates, 2, "general");
    assert!(req.requires_shadow, "Y2 should require shadow eval");

    // 2. Kanban item has high priority
    let action = on_proposal_created(
        "prop-002",
        "Modify inference rules",
        "santiago",
        "reasoning_change",
        &req,
    );
    match &action {
        KanbanAction::CreateItem { priority, .. } => {
            assert_eq!(priority, "high"); // Y2 = high priority
        }
        _ => panic!("Expected CreateItem"),
    }

    // 3-4. Shadow eval shows regression, proposal rejected
    let reject_action = on_proposal_transition("prop-002", &ProposalStatus::Rejected);
    match &reject_action {
        KanbanAction::Complete { outcome, .. } => assert_eq!(outcome, "rejected"),
        _ => panic!("Expected Complete"),
    }

    // 5-6. EXPR with REFUTED
    let record = generate_experiment_record(
        "prop-002",
        "Modify inference rules",
        "santiago",
        "lowering inference threshold from 0.7 to 0.5",
        2,
        "general",
        "Modified 3 Y2 inference rules",
        "Y2 default: do-calculus gate",
        vec![MetricComparison {
            name: "cq_coverage".to_string(),
            baseline: 0.85,
            shadow: 0.78,
            delta: -0.07,
        }],
        ExperimentOutcome::Refuted,
        "CQ coverage regressed 7% — threshold too low",
        2,
    );
    assert_eq!(record.outcome, ExperimentOutcome::Refuted);
    let md = render_experiment_markdown(&record);
    assert!(md.contains("**REFUTED**"));
}

/// Test scenario 3: Hypothesis accumulation
///
/// Three experiments test the same hypothesis — 2 validate, 1 refutes.
/// Evidence shows "2/3 experiments validated".
#[test]
fn test_scenario_3_hypothesis_accumulation() {
    let experiments = vec![
        ("EXPR-auto-001".to_string(), ExperimentOutcome::Validated),
        ("EXPR-auto-002".to_string(), ExperimentOutcome::Refuted),
        ("EXPR-auto-003".to_string(), ExperimentOutcome::Validated),
    ];

    let evidence = accumulate_evidence("H-024", &experiments);
    assert_eq!(evidence.total_experiments, 3);
    assert_eq!(evidence.validated_count, 2);
    assert_eq!(evidence.refuted_count, 1);
    assert_eq!(evidence.summary(), "2/3 experiments validated");
    assert!(evidence.has_strong_evidence());

    // Verify all cross-board links
    for (exp_id, _) in &experiments {
        let links = generate_cross_board_links("prop-xxx", exp_id, Some("H-024"));
        assert_eq!(links.len(), 3);
        // Every experiment links to the hypothesis
        assert!(links.iter().any(|l| l.predicate == "kb:testedHypothesis"));
        assert!(
            links
                .iter()
                .any(|l| l.subject == "H-024" && l.predicate == "kb:evidencedBy")
        );
    }
}

/// Test scenario 4: Concurrent proposals (independent state)
///
/// Two proposals created simultaneously — merge one, reject the other.
/// Both produce correct EXPR items with no cross-contamination.
#[test]
fn test_scenario_4_concurrent_proposals() {
    let gates = default_gates().unwrap();

    // Two proposals created
    let req_y0 = classify_change(&gates, 0, "general");
    let req_y5 = classify_change(&gates, 5, "medical");

    let action_a = on_proposal_created("prop-A", "Add facts", "santiago", "knowledge", &req_y0);
    let action_b = on_proposal_created(
        "prop-B",
        "Update procedure",
        "santiago",
        "procedure",
        &req_y5,
    );

    // Verify independent kanban items
    match (&action_a, &action_b) {
        (
            KanbanAction::CreateItem {
                id: id_a,
                priority: p_a,
                ..
            },
            KanbanAction::CreateItem {
                id: id_b,
                priority: p_b,
                ..
            },
        ) => {
            assert_ne!(id_a, id_b);
            assert_eq!(p_a, "low"); // Y0 auto-approve
            assert_eq!(p_b, "high"); // Y5 medical
        }
        _ => panic!("Expected two CreateItem actions"),
    }

    // Merge A, reject B
    let merge_a = on_proposal_transition("prop-A", &ProposalStatus::Merged);
    let reject_b = on_proposal_transition("prop-B", &ProposalStatus::Rejected);

    match (&merge_a, &reject_b) {
        (
            KanbanAction::Complete { outcome: oa, .. },
            KanbanAction::Complete { outcome: ob, .. },
        ) => {
            assert_eq!(oa, "merged");
            assert_eq!(ob, "rejected");
        }
        _ => panic!("Expected two Complete actions"),
    }

    // Both produce independent EXPR items
    let expr_a = generate_experiment_record(
        "prop-A",
        "Add facts",
        "santiago",
        "adding facts",
        0,
        "general",
        "15 triples",
        "auto",
        vec![],
        ExperimentOutcome::Validated,
        "Passed",
        1,
    );
    let expr_b = generate_experiment_record(
        "prop-B",
        "Update procedure",
        "santiago",
        "updating procedure",
        5,
        "medical",
        "3 rules",
        "Y5 medical",
        vec![],
        ExperimentOutcome::Refuted,
        "Regression",
        2,
    );

    assert_ne!(expr_a.experiment_id, expr_b.experiment_id);
    assert_eq!(expr_a.outcome, ExperimentOutcome::Validated);
    assert_eq!(expr_b.outcome, ExperimentOutcome::Refuted);
}

/// Test: Y6 proposal gets critical priority and human-gate tag
#[test]
fn test_y6_proposal_critical_priority() {
    let gates = default_gates().unwrap();
    let req = classify_change(&gates, 6, "general");
    assert_eq!(safety_to_priority(&req), "critical");

    let action = on_proposal_created(
        "prop-y6",
        "Adjust calibration",
        "santiago",
        "metacognition",
        &req,
    );
    match action {
        KanbanAction::CreateItem { priority, tags, .. } => {
            assert_eq!(priority, "critical");
            assert!(tags.contains(&"human-gate".to_string()));
        }
        _ => panic!("Expected CreateItem"),
    }
}
