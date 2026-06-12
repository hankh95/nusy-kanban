//! Extended safety gate tests — full matrix coverage, edge cases, and
//! multi-step review workflows.
//!
//! EXP-3002 Phase 5.

use nusy_graph_review::{
    ApprovalRequirement, CommentStore, CreateProposalInput, ProposalStatus, ProposalStore,
    ShadowEvalResult, check_approval_gate, check_merge_gate, classify_change,
    classify_proposal_changes, default_gates,
};

// ─── Safety Gate Matrix: Full Y-Layer Coverage ──────────────────────────────

/// Verify the default gate matrix has the documented 12 rules.
#[test]
fn test_default_gates_has_12_rules() {
    let gates = default_gates().expect("default gates");
    // 12 rules as documented: y0-default, y0-medical, y0-legal, y1-default,
    // y1-medical, y2-default, y3-default, y4-default, y5-default,
    // y5-medical, y5-legal, y6-default
    assert!(gates.len() >= 12, "at least 12 gate rules");
}

/// Y0 general → auto-approve (no human, no shadow)
#[test]
fn test_y0_general_auto_approve() {
    let gates = default_gates().expect("gates");
    let req = classify_change(&gates, 0, "general");
    assert!(!req.requires_human, "Y0 general: no human");
    assert!(!req.requires_shadow, "Y0 general: no shadow");
    assert_eq!(req.auto_approve_threshold, 0.0);
}

/// Y0 medical → requires shadow eval
#[test]
fn test_y0_medical_requires_shadow() {
    let gates = default_gates().expect("gates");
    let req = classify_change(&gates, 0, "medical");
    assert!(!req.requires_human, "Y0 medical: no human");
    assert!(req.requires_shadow, "Y0 medical: requires shadow");
}

/// Y0 legal → requires shadow eval
#[test]
fn test_y0_legal_requires_shadow() {
    let gates = default_gates().expect("gates");
    let req = classify_change(&gates, 0, "legal");
    assert!(req.requires_shadow, "Y0 legal: requires shadow");
}

/// Y2 (reasoning) always requires shadow
#[test]
fn test_y2_always_requires_shadow() {
    let gates = default_gates().expect("gates");
    let req = classify_change(&gates, 2, "general");
    assert!(req.requires_shadow, "Y2: requires shadow");
    assert!(!req.requires_human, "Y2: no human needed");
}

/// Y6 (metacognitive) ALWAYS requires human — the non-negotiable rule
#[test]
fn test_y6_always_requires_human() {
    let gates = default_gates().expect("gates");
    let req = classify_change(&gates, 6, "general");
    assert!(req.requires_human, "Y6 MUST require human");
    assert!(req.requires_shadow, "Y6 also requires shadow");
}

/// Y3 (experience) and Y4 (journal) are auto-approve
#[test]
fn test_y3_y4_auto_approve() {
    let gates = default_gates().expect("gates");
    let y3 = classify_change(&gates, 3, "general");
    let y4 = classify_change(&gates, 4, "general");
    assert!(!y3.requires_human && !y3.requires_shadow, "Y3 auto-approve");
    assert!(!y4.requires_human && !y4.requires_shadow, "Y4 auto-approve");
}

/// Y5 general requires shadow, Y5 medical/legal has higher threshold
#[test]
fn test_y5_domain_escalation() {
    let gates = default_gates().expect("gates");
    let general = classify_change(&gates, 5, "general");
    let medical = classify_change(&gates, 5, "medical");
    let legal = classify_change(&gates, 5, "legal");

    assert!(general.requires_shadow);
    assert!(medical.requires_shadow);
    assert!(legal.requires_shadow);

    // Medical and legal should have higher threshold
    assert!(
        medical.auto_approve_threshold >= general.auto_approve_threshold,
        "medical threshold >= general"
    );
}

/// Unknown domain falls back to wildcard
#[test]
fn test_unknown_domain_uses_wildcard() {
    let gates = default_gates().expect("gates");
    let req = classify_change(&gates, 1, "education");
    // Should match y1-default (wildcard)
    assert!(!req.requires_human);
}

// ─── Batch Classification (Most Restrictive Wins) ───────────────────────────

/// When changes span multiple Y-layers, most restrictive requirement wins
#[test]
fn test_batch_classification_most_restrictive() {
    use nusy_graph_review::safety_gates::ChangeEntry;

    let gates = default_gates().expect("gates");
    let changes = vec![
        ChangeEntry {
            y_layer: 0,
            domain: "general".to_string(),
        }, // auto-approve
        ChangeEntry {
            y_layer: 6,
            domain: "general".to_string(),
        }, // requires human
    ];

    let req = classify_proposal_changes(&gates, &changes);
    assert!(
        req.requires_human,
        "batch with Y6 change must require human"
    );
}

/// Single auto-approve change should not require anything
#[test]
fn test_batch_single_safe_change() {
    use nusy_graph_review::safety_gates::ChangeEntry;

    let gates = default_gates().expect("gates");
    let changes = vec![ChangeEntry {
        y_layer: 3,
        domain: "general".to_string(),
    }];

    let req = classify_proposal_changes(&gates, &changes);
    assert!(!req.requires_human);
    assert!(!req.requires_shadow);
}

// ─── Approval Gate Checks ───────────────────────────────────────────────────

/// Agent can approve when no human is required
#[test]
fn test_agent_can_approve_non_human_gate() {
    let req = ApprovalRequirement {
        requires_human: false,
        requires_shadow: false,
        auto_approve_threshold: 0.0,
        gate_id: "y0-default".to_string(),
        description: "Auto-approve".to_string(),
    };

    let result = check_approval_gate(&req, false, None);
    assert!(result.is_ok(), "agent can approve auto-approve gate");
}

/// Agent CANNOT approve when human is required (Y6)
#[test]
fn test_agent_cannot_approve_human_gate() {
    let req = ApprovalRequirement {
        requires_human: true,
        requires_shadow: true,
        auto_approve_threshold: 0.10,
        gate_id: "y6-default".to_string(),
        description: "Human required".to_string(),
    };

    let result = check_approval_gate(&req, false, None);
    assert!(result.is_err(), "agent blocked from approving Y6 gate");
}

/// Human CAN approve Y6 gates
#[test]
fn test_human_can_approve_human_gate() {
    let req = ApprovalRequirement {
        requires_human: true,
        requires_shadow: true,
        auto_approve_threshold: 0.10,
        gate_id: "y6-default".to_string(),
        description: "Human required".to_string(),
    };

    let shadow = ShadowEvalResult {
        metric_improvement: 0.15,
        proof_gates_passed: true,
    };
    let result = check_approval_gate(&req, true, Some(&shadow));
    assert!(result.is_ok(), "human can approve Y6 gate with shadow eval");
}

/// Shadow eval below threshold blocks approval
#[test]
fn test_shadow_below_threshold_blocks() {
    let req = ApprovalRequirement {
        requires_human: false,
        requires_shadow: true,
        auto_approve_threshold: 0.05,
        gate_id: "y2-default".to_string(),
        description: "Shadow required".to_string(),
    };

    let shadow = ShadowEvalResult {
        metric_improvement: 0.01, // below 0.05 threshold
        proof_gates_passed: true,
    };
    let result = check_approval_gate(&req, false, Some(&shadow));
    assert!(result.is_err(), "shadow below threshold blocks approval");
}

/// Shadow eval at exactly threshold passes
#[test]
fn test_shadow_at_threshold_passes() {
    let req = ApprovalRequirement {
        requires_human: false,
        requires_shadow: true,
        auto_approve_threshold: 0.05,
        gate_id: "y2-default".to_string(),
        description: "Shadow required".to_string(),
    };

    let shadow = ShadowEvalResult {
        metric_improvement: 0.05, // exactly at threshold
        proof_gates_passed: true,
    };
    let result = check_approval_gate(&req, false, Some(&shadow));
    assert!(result.is_ok(), "shadow at threshold passes");
}

// ─── Merge Gate Checks ──────────────────────────────────────────────────────

/// Merge succeeds when proof gates pass
#[test]
fn test_merge_gate_proof_passes() {
    let req = ApprovalRequirement {
        requires_human: false,
        requires_shadow: true,
        auto_approve_threshold: 0.05,
        gate_id: "y2-default".to_string(),
        description: "".to_string(),
    };

    let shadow = ShadowEvalResult {
        metric_improvement: 0.10,
        proof_gates_passed: true,
    };
    let result = check_merge_gate(&req, Some(&shadow));
    assert!(result.is_ok(), "merge passes when proof gates pass");
}

/// Merge blocked when proof gates fail
#[test]
fn test_merge_gate_proof_fails() {
    let req = ApprovalRequirement {
        requires_human: false,
        requires_shadow: true,
        auto_approve_threshold: 0.05,
        gate_id: "y2-default".to_string(),
        description: "".to_string(),
    };

    let shadow = ShadowEvalResult {
        metric_improvement: 0.10,
        proof_gates_passed: false, // proof gates failed
    };
    let result = check_merge_gate(&req, Some(&shadow));
    assert!(result.is_err(), "merge blocked when proof gates fail");
}

// ─── Comment Threading ──────────────────────────────────────────────────────

#[test]
fn test_threaded_comments_unresolved_count() {
    let mut comments = CommentStore::new();
    let prop_id = "PROP-2001";

    // Add root comment
    let root = comments
        .add_comment(prop_id, "reviewer1", "Issue found", None, None)
        .expect("root comment");

    // Add reply to root (should NOT count toward unresolved)
    let _reply = comments
        .add_comment(prop_id, "author", "Fixed", None, Some(&root))
        .expect("reply");

    // Only root comments count
    let count = comments.unresolved_count(prop_id).expect("count");
    assert_eq!(count, 1, "only root comments count as unresolved");

    // Resolve root
    comments.resolve_comment(&root).expect("resolve");
    let count = comments
        .unresolved_count(prop_id)
        .expect("count after resolve");
    assert_eq!(count, 0, "resolved root clears count");
}

#[test]
fn test_multiple_root_comments() {
    let mut comments = CommentStore::new();
    let prop_id = "PROP-2002";

    comments
        .add_comment(prop_id, "r1", "Issue A", None, None)
        .expect("comment 1");
    comments
        .add_comment(prop_id, "r2", "Issue B", None, None)
        .expect("comment 2");
    comments
        .add_comment(prop_id, "r3", "Issue C", None, None)
        .expect("comment 3");

    let count = comments.unresolved_count(prop_id).expect("count");
    assert_eq!(count, 3, "three unresolved root comments");
}

// ─── Full Proposal Lifecycle with Safety Gates ──────────────────────────────

#[test]
fn test_full_proposal_lifecycle_with_gates() {
    let mut proposals = ProposalStore::new();
    let mut comments = CommentStore::new();

    // Create proposal
    let prop_id = proposals
        .create_proposal(&CreateProposalInput {
            author: "being-alpha",
            title: "Add new reasoning rule",
            source_branch: "proposal/reasoning-rule",
            target_branch: "main",
            namespace: "self",
            proposal_type: "knowledge_change",
            description: Some("Adding Y2 reasoning constraint"),
        })
        .expect("create");

    assert_eq!(
        proposals.get_status(&prop_id).unwrap(),
        ProposalStatus::Draft
    );

    // Open
    proposals.open_proposal(&prop_id).expect("open");
    assert_eq!(
        proposals.get_status(&prop_id).unwrap(),
        ProposalStatus::Open
    );

    // Add reviewer
    proposals
        .add_reviewer(&prop_id, "reviewer-beta")
        .expect("add reviewer");
    assert_eq!(
        proposals.get_status(&prop_id).unwrap(),
        ProposalStatus::Reviewing
    );

    // Add comment (blocks approval if unresolved)
    let comment_id = comments
        .add_comment(
            &prop_id,
            "reviewer-beta",
            "Please clarify the constraint",
            None,
            None,
        )
        .expect("comment");

    // Approval would be blocked by unresolved comment
    let unresolved = comments.unresolved_count(&prop_id).expect("count");
    assert_eq!(unresolved, 1, "one unresolved comment blocks approval");

    // Resolve comment
    comments.resolve_comment(&comment_id).expect("resolve");
    assert_eq!(comments.unresolved_count(&prop_id).unwrap(), 0);

    // Approve
    proposals
        .approve(&prop_id, "reviewer-beta", 0)
        .expect("approve");
    assert_eq!(
        proposals.get_status(&prop_id).unwrap(),
        ProposalStatus::Approved
    );

    // Merge
    proposals
        .mark_merged(&prop_id, "reviewer-beta", None, None)
        .expect("merge");
    assert_eq!(
        proposals.get_status(&prop_id).unwrap(),
        ProposalStatus::Merged
    );
}

// ─── Reject → Revise → Approve Cycle ───────────────────────────────────────

#[test]
fn test_reject_revise_approve_cycle() {
    let mut proposals = ProposalStore::new();

    let prop_id = proposals
        .create_proposal(&CreateProposalInput {
            author: "being-alpha",
            title: "Rejected then revised",
            source_branch: "proposal/revise-test",
            target_branch: "main",
            namespace: "work",
            proposal_type: "code_change",
            description: None,
        })
        .expect("create");

    proposals.open_proposal(&prop_id).expect("open");
    proposals
        .add_reviewer(&prop_id, "reviewer")
        .expect("add reviewer");

    // Reject
    proposals.reject(&prop_id, "reviewer").expect("reject");
    assert_eq!(
        proposals.get_status(&prop_id).unwrap(),
        ProposalStatus::Rejected
    );

    // Revise (auto-advances to Reviewing)
    proposals.revise(&prop_id, "being-alpha").expect("revise");
    assert_eq!(
        proposals.get_status(&prop_id).unwrap(),
        ProposalStatus::Reviewing
    );

    // Now approve
    proposals.approve(&prop_id, "reviewer", 0).expect("approve");
    assert_eq!(
        proposals.get_status(&prop_id).unwrap(),
        ProposalStatus::Approved
    );
}

// ─── Close Without Merging ──────────────────────────────────────────────────

#[test]
fn test_close_without_merging() {
    let mut proposals = ProposalStore::new();

    let prop_id = proposals
        .create_proposal(&CreateProposalInput {
            author: "being-alpha",
            title: "Will be closed",
            source_branch: "proposal/close-test",
            target_branch: "main",
            namespace: "research",
            proposal_type: "knowledge_change",
            description: None,
        })
        .expect("create");

    proposals.open_proposal(&prop_id).expect("open");
    proposals
        .close_proposal(&prop_id, "being-alpha", None)
        .expect("close");
    assert_eq!(
        proposals.get_status(&prop_id).unwrap(),
        ProposalStatus::Closed
    );
}
