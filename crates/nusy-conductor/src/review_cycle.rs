//! Review cycle automation — PR detection, reviewer assignment, approval routing.
//!
//! Automates the Captain's manual handoff:
//! Agent A `/workit` → PR → Conductor assigns Agent B `/reviewit` → result →
//! if clean + auto-approvable: auto-approve, else escalate to Captain.
//!
//! Captain policy: Voyages/expeditions require Captain approval.
//! Chores/signals/hazards/docs are auto-approvable.

use crate::state::AssigneeTracker;
use std::collections::HashMap;

// ─── PR Classification ──────────────────────────────────────────────────────

/// Whether a PR requires Captain approval or can be auto-approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Captain must approve (voyages, expeditions).
    CaptainRequired,
    /// Can be auto-approved after reviewer approves (chores, signals, etc.).
    AutoApprovable,
}

impl std::fmt::Display for ApprovalPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CaptainRequired => f.write_str("captain-required"),
            Self::AutoApprovable => f.write_str("auto-approvable"),
        }
    }
}

/// Classify a work item's approval policy based on its type.
///
/// Captain policy (2026-03-16):
/// - `voyage`, `expedition` → Captain required
/// - `chore`, `signal`, `hazard`, docs-only → auto-approvable
/// - Unknown type → Captain required (conservative default)
pub fn classify_approval(item_type: &str) -> ApprovalPolicy {
    match item_type.to_lowercase().as_str() {
        "chore" | "signal" | "hazard" => ApprovalPolicy::AutoApprovable,
        "voyage" | "expedition" => ApprovalPolicy::CaptainRequired,
        _ => ApprovalPolicy::CaptainRequired, // conservative default
    }
}

/// Extract a work item ID from a PR title or body.
///
/// Matches patterns: EX-NNNN, EXP-NNNN, CH-NNNN, VY-NNNN, VOY-NNNN, etc.
pub fn extract_item_id(text: &str) -> Option<String> {
    // Common prefixes in priority order
    let prefixes = [
        "EX-", "EXP-", "CH-", "CHORE-", "VY-", "VOY-", "HAZ-", "SIG-",
    ];

    for prefix in prefixes {
        if let Some(pos) = text.find(prefix) {
            let rest = &text[pos + prefix.len()..];
            let num_end = rest
                .find(|c: char| !c.is_ascii_digit() && c != '.')
                .unwrap_or(rest.len());
            if num_end > 0 {
                return Some(format!("{}{}", prefix, &rest[..num_end]));
            }
        }
    }

    None
}

// ─── Proposal Tracking ──────────────────────────────────────────────────────

/// A proposal detected by the conductor.
#[derive(Debug, Clone)]
pub struct TrackedProposal {
    /// Proposal ID (e.g., "PROP-2020").
    pub proposal_id: String,
    /// Associated work item ID (e.g., "EX-3047"), if matched.
    pub item_id: Option<String>,
    /// Work item type (e.g., "expedition"), if known.
    pub item_type: Option<String>,
    /// PR title.
    pub title: String,
    /// Source branch for CI checks.
    pub source_branch: String,
    /// Author (implementer).
    pub author: String,
    /// Approval policy for this PR.
    pub policy: ApprovalPolicy,
    /// Current state in the review cycle.
    pub cycle_state: CycleState,
    /// Assigned reviewer, if any.
    pub reviewer: Option<String>,
    /// Number of review rounds completed.
    pub review_rounds: u32,
}

/// State of a PR in the review cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleState {
    /// Detected, awaiting CI checks before reviewer assignment.
    AwaitingCi,
    /// CI passed, awaiting reviewer assignment.
    AwaitingReviewer,
    /// Reviewer assigned, awaiting review result.
    InReview,
    /// Changes requested, routed back to implementer.
    ChangesRequested,
    /// Re-review in progress (after implementer fixes).
    ReReview,
    /// Review approved, awaiting merge decision.
    Approved,
    /// Auto-approved and merged.
    Merged,
    /// Escalated to Captain (max rounds or Captain-required).
    EscalatedToCaptain,
}

impl std::fmt::Display for CycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AwaitingCi => f.write_str("awaiting-ci"),
            Self::AwaitingReviewer => f.write_str("awaiting-reviewer"),
            Self::InReview => f.write_str("in-review"),
            Self::ChangesRequested => f.write_str("changes-requested"),
            Self::ReReview => f.write_str("re-review"),
            Self::Approved => f.write_str("approved"),
            Self::Merged => f.write_str("merged"),
            Self::EscalatedToCaptain => f.write_str("escalated-to-captain"),
        }
    }
}

/// Maximum review rounds before escalation.
const MAX_REVIEW_ROUNDS: u32 = 2;

// ─── Review Cycle Engine ────────────────────────────────────────────────────

/// The review cycle engine — orchestrates PR detection, assignment, and routing.
pub struct ReviewCycleEngine {
    /// Active proposals being tracked.
    tracked: HashMap<String, TrackedProposal>,
    /// Agent tracker for reviewer selection.
    tracker: AssigneeTracker,
}

/// An action the conductor should take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConductorAction {
    /// Trigger CI checks for a proposal (send to conductor.ci.request).
    TriggerCi { proposal_id: String, branch: String },
    /// Assign a reviewer to a proposal.
    AssignReviewer {
        proposal_id: String,
        reviewer: String,
        reason: String,
    },
    /// Route changes back to the implementer with CI failure details.
    RouteToImplementer {
        proposal_id: String,
        implementer: String,
        round: u32,
    },
    /// Auto-approve and merge a proposal.
    AutoApprove { proposal_id: String },
    /// Escalate to Captain for approval.
    EscalateToCaptain { proposal_id: String, reason: String },
    /// Log an orchestration decision.
    LogDecision {
        proposal_id: String,
        message: String,
    },
}

impl ReviewCycleEngine {
    /// Create a new engine with default agent profiles.
    pub fn new() -> Self {
        ReviewCycleEngine {
            tracked: HashMap::new(),
            tracker: AssigneeTracker::with_defaults(),
        }
    }

    /// Create with custom agent tracker.
    pub fn with_tracker(tracker: AssigneeTracker) -> Self {
        ReviewCycleEngine {
            tracked: HashMap::new(),
            tracker,
        }
    }

    /// Get all tracked proposals.
    pub fn tracked_proposals(&self) -> &HashMap<String, TrackedProposal> {
        &self.tracked
    }

    /// Register a new proposal detected from NATS or GitHub.
    ///
    /// Returns actions to take: first triggers CI, then waits for CI result
    /// before assigning a reviewer.
    #[allow(clippy::too_many_arguments)]
    pub fn on_proposal_detected(
        &mut self,
        proposal_id: &str,
        title: &str,
        source_branch: &str,
        author: &str,
        item_id: Option<&str>,
        item_type: Option<&str>,
        _work_graph: &crate::reader::WorkGraph,
    ) -> Vec<ConductorAction> {
        let mut actions = Vec::new();

        // Classify approval policy
        let policy = item_type
            .map(classify_approval)
            .unwrap_or(ApprovalPolicy::CaptainRequired);

        let proposal = TrackedProposal {
            proposal_id: proposal_id.to_string(),
            item_id: item_id.map(String::from),
            item_type: item_type.map(String::from),
            title: title.to_string(),
            source_branch: source_branch.to_string(),
            author: author.to_string(),
            policy,
            cycle_state: CycleState::AwaitingCi,
            reviewer: None,
            review_rounds: 0,
        };

        actions.push(ConductorAction::LogDecision {
            proposal_id: proposal_id.to_string(),
            message: format!(
                "Detected proposal: {} ({}) — policy: {}, triggering CI",
                title,
                item_type.unwrap_or("unknown"),
                policy,
            ),
        });

        // Trigger CI checks — reviewer assignment happens in on_ci_result
        actions.push(ConductorAction::TriggerCi {
            proposal_id: proposal_id.to_string(),
            branch: source_branch.to_string(),
        });

        self.tracked.insert(proposal_id.to_string(), proposal);

        actions
    }

    /// Handle a review result (approved or changes requested).
    pub fn on_review_result(&mut self, proposal_id: &str, approved: bool) -> Vec<ConductorAction> {
        let mut actions = Vec::new();

        let Some(proposal) = self.tracked.get_mut(proposal_id) else {
            actions.push(ConductorAction::LogDecision {
                proposal_id: proposal_id.to_string(),
                message: "review result for untracked proposal — ignoring".to_string(),
            });
            return actions;
        };

        proposal.review_rounds += 1;

        if approved {
            proposal.cycle_state = CycleState::Approved;

            match proposal.policy {
                ApprovalPolicy::AutoApprovable => {
                    proposal.cycle_state = CycleState::Merged;
                    actions.push(ConductorAction::AutoApprove {
                        proposal_id: proposal_id.to_string(),
                    });
                    actions.push(ConductorAction::LogDecision {
                        proposal_id: proposal_id.to_string(),
                        message: format!(
                            "Auto-approved after {} review round(s)",
                            proposal.review_rounds
                        ),
                    });
                }
                ApprovalPolicy::CaptainRequired => {
                    proposal.cycle_state = CycleState::EscalatedToCaptain;
                    actions.push(ConductorAction::EscalateToCaptain {
                        proposal_id: proposal_id.to_string(),
                        reason: format!(
                            "reviewer approved, but {} requires Captain approval",
                            proposal.item_type.as_deref().unwrap_or("unknown type")
                        ),
                    });
                }
            }
        } else {
            // Changes requested
            if proposal.review_rounds >= MAX_REVIEW_ROUNDS {
                proposal.cycle_state = CycleState::EscalatedToCaptain;
                actions.push(ConductorAction::EscalateToCaptain {
                    proposal_id: proposal_id.to_string(),
                    reason: format!(
                        "max review rounds ({MAX_REVIEW_ROUNDS}) reached without approval"
                    ),
                });
            } else {
                proposal.cycle_state = CycleState::ChangesRequested;
                actions.push(ConductorAction::RouteToImplementer {
                    proposal_id: proposal_id.to_string(),
                    implementer: proposal.author.clone(),
                    round: proposal.review_rounds,
                });
            }
        }

        actions
    }

    /// Handle fixes pushed by the implementer — triggers re-review.
    pub fn on_fixes_pushed(&mut self, proposal_id: &str) -> Vec<ConductorAction> {
        let mut actions = Vec::new();

        let Some(proposal) = self.tracked.get_mut(proposal_id) else {
            return actions;
        };

        if proposal.cycle_state != CycleState::ChangesRequested {
            actions.push(ConductorAction::LogDecision {
                proposal_id: proposal_id.to_string(),
                message: format!(
                    "fixes pushed but proposal is in {} state — ignoring",
                    proposal.cycle_state
                ),
            });
            return actions;
        }

        proposal.cycle_state = CycleState::ReReview;

        if let Some(ref reviewer) = proposal.reviewer {
            actions.push(ConductorAction::AssignReviewer {
                proposal_id: proposal_id.to_string(),
                reviewer: reviewer.clone(),
                reason: format!("re-review round {}", proposal.review_rounds + 1),
            });
        }

        actions
    }

    /// Handle a CI result — either proceed to reviewer assignment or route back
    /// to implementer if CI failed.
    ///
    /// This is called when the CI service publishes a result to `conductor.ci.result`.
    pub fn on_ci_result(
        &mut self,
        proposal_id: &str,
        passed: bool,
        summary: &str,
        work_graph: &crate::reader::WorkGraph,
    ) -> Vec<ConductorAction> {
        let mut actions = Vec::new();

        // Phase 1: Read-only checks — extract author without holding mutable borrow
        let author = {
            let Some(proposal) = self.tracked.get(proposal_id) else {
                actions.push(ConductorAction::LogDecision {
                    proposal_id: proposal_id.to_string(),
                    message: "CI result for untracked proposal — ignoring".to_string(),
                });
                return actions;
            };

            if proposal.cycle_state != CycleState::AwaitingCi {
                actions.push(ConductorAction::LogDecision {
                    proposal_id: proposal_id.to_string(),
                    message: format!(
                        "CI result received but proposal is in {} state — ignoring",
                        proposal.cycle_state
                    ),
                });
                return actions;
            }

            proposal.author.clone()
        };

        // Phase 2: Select reviewer (borrows self immutably)
        let suggestion = if passed {
            self.select_reviewer(&author, work_graph)
        } else {
            None
        };

        // Phase 3: Mutate tracked proposal
        let proposal = self.tracked.get_mut(proposal_id).unwrap();

        if passed {
            actions.push(ConductorAction::LogDecision {
                proposal_id: proposal_id.to_string(),
                message: format!("CI passed: {summary}"),
            });

            if let Some(reviewer) = suggestion {
                actions.push(ConductorAction::AssignReviewer {
                    proposal_id: proposal_id.to_string(),
                    reviewer: reviewer.clone(),
                    reason: format!("CI passed, available agent != {author}"),
                });
                proposal.reviewer = Some(reviewer);
                proposal.cycle_state = CycleState::InReview;
            } else {
                actions.push(ConductorAction::EscalateToCaptain {
                    proposal_id: proposal_id.to_string(),
                    reason: "CI passed but no available reviewer".to_string(),
                });
                proposal.cycle_state = CycleState::EscalatedToCaptain;
            }
        } else {
            actions.push(ConductorAction::LogDecision {
                proposal_id: proposal_id.to_string(),
                message: format!("CI failed: {summary}"),
            });
            actions.push(ConductorAction::RouteToImplementer {
                proposal_id: proposal_id.to_string(),
                implementer: author,
                round: 0, // CI failure doesn't count as a review round
            });
            proposal.cycle_state = CycleState::ChangesRequested;
        }

        actions
    }

    /// Select a reviewer that is not the author.
    fn select_reviewer(
        &self,
        author: &str,
        work_graph: &crate::reader::WorkGraph,
    ) -> Option<String> {
        let available = self.tracker.available_agents(work_graph);
        // Prefer available agents who aren't the author
        let candidates: Vec<_> = available.iter().filter(|a| a.name != author).collect();

        if let Some(agent) = candidates.first() {
            return Some(agent.name.clone());
        }

        // Fallback: any agent that isn't the author (even if busy)
        let all_states = self.tracker.agent_states(work_graph);
        all_states
            .iter()
            .filter(|s| s.profile.name != author)
            .min_by_key(|s| s.in_progress.len())
            .map(|s| s.profile.name.clone())
    }
}

impl Default for ReviewCycleEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{WorkGraph, WorkItem};

    fn make_item(id: &str, status: &str, assignee: Option<&str>) -> WorkItem {
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

    fn empty_graph() -> WorkGraph {
        WorkGraph::from_items(vec![])
    }

    fn graph_with_busy_m5() -> WorkGraph {
        WorkGraph::from_items(vec![make_item("EX-100", "in_progress", Some("M5"))])
    }

    // ── Classification tests ────────────────────────────────────────────

    #[test]
    fn test_classify_expedition_requires_captain() {
        assert_eq!(
            classify_approval("expedition"),
            ApprovalPolicy::CaptainRequired
        );
    }

    #[test]
    fn test_classify_voyage_requires_captain() {
        assert_eq!(classify_approval("voyage"), ApprovalPolicy::CaptainRequired);
    }

    #[test]
    fn test_classify_chore_auto_approvable() {
        assert_eq!(classify_approval("chore"), ApprovalPolicy::AutoApprovable);
    }

    #[test]
    fn test_classify_signal_auto_approvable() {
        assert_eq!(classify_approval("signal"), ApprovalPolicy::AutoApprovable);
    }

    #[test]
    fn test_classify_unknown_defaults_captain() {
        assert_eq!(
            classify_approval("unknown_type"),
            ApprovalPolicy::CaptainRequired
        );
    }

    #[test]
    fn test_classify_case_insensitive() {
        assert_eq!(
            classify_approval("Expedition"),
            ApprovalPolicy::CaptainRequired
        );
        assert_eq!(classify_approval("CHORE"), ApprovalPolicy::AutoApprovable);
    }

    // ── ID extraction tests ─────────────────────────────────────────────

    #[test]
    fn test_extract_item_id_from_title() {
        assert_eq!(
            extract_item_id("EX-3047: Conductor Foundation"),
            Some("EX-3047".to_string())
        );
    }

    #[test]
    fn test_extract_item_id_chore() {
        assert_eq!(
            extract_item_id("CH-3020: Add pr resolve"),
            Some("CH-3020".to_string())
        );
    }

    #[test]
    fn test_extract_item_id_voyage() {
        assert_eq!(
            extract_item_id("VY-3010 PR1: semantic diff"),
            Some("VY-3010".to_string())
        );
    }

    #[test]
    fn test_extract_item_id_none() {
        assert_eq!(extract_item_id("Fix a bug"), None);
    }

    #[test]
    fn test_extract_item_id_in_body() {
        assert_eq!(
            extract_item_id("## Expedition\nEXP-1133: Title here"),
            Some("EXP-1133".to_string())
        );
    }

    // ── Helper: detect + pass CI (many tests need this) ────────────────

    /// Detect a proposal and pass CI, returning the post-CI actions.
    fn detect_and_pass_ci(
        engine: &mut ReviewCycleEngine,
        proposal_id: &str,
        title: &str,
        author: &str,
        item_id: Option<&str>,
        item_type: Option<&str>,
        graph: &WorkGraph,
    ) -> Vec<ConductorAction> {
        engine.on_proposal_detected(
            proposal_id,
            title,
            "main",
            author,
            item_id,
            item_type,
            graph,
        );
        engine.on_ci_result(proposal_id, true, "all passed", graph)
    }

    // ── Review cycle engine tests ───────────────────────────────────────

    #[test]
    fn test_proposal_detected_triggers_ci() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        let actions = engine.on_proposal_detected(
            "PROP-2020",
            "EX-3047: Conductor",
            "expedition/ex-3047",
            "M5",
            Some("EX-3047"),
            Some("expedition"),
            &graph,
        );

        // Should have LogDecision + TriggerCi
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::TriggerCi { .. }))
        );

        let proposal = engine.tracked.get("PROP-2020").expect("tracked");
        assert_eq!(proposal.cycle_state, CycleState::AwaitingCi);
        assert_eq!(proposal.source_branch, "expedition/ex-3047");
    }

    #[test]
    fn test_ci_pass_assigns_reviewer() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        let actions = detect_and_pass_ci(
            &mut engine,
            "PROP-2020",
            "EX-3047: Conductor",
            "M5",
            Some("EX-3047"),
            Some("expedition"),
            &graph,
        );

        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::AssignReviewer { .. }))
        );

        let proposal = engine.tracked.get("PROP-2020").expect("tracked");
        assert_eq!(proposal.cycle_state, CycleState::InReview);
        assert!(proposal.reviewer.is_some());
        assert_ne!(proposal.reviewer.as_deref(), Some("M5")); // not self-review
    }

    #[test]
    fn test_ci_fail_routes_to_implementer() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        engine.on_proposal_detected(
            "PROP-2020",
            "EX-3047: Conductor",
            "main",
            "M5",
            Some("EX-3047"),
            Some("expedition"),
            &graph,
        );

        let actions = engine.on_ci_result("PROP-2020", false, "3 tests failed", &graph);

        // Should route back to implementer
        assert!(actions.iter().any(|a| matches!(
            a,
            ConductorAction::RouteToImplementer { implementer, .. }
            if implementer == "M5"
        )));

        let proposal = engine.tracked.get("PROP-2020").expect("tracked");
        assert_eq!(proposal.cycle_state, CycleState::ChangesRequested);
    }

    #[test]
    fn test_ci_result_untracked_proposal_ignored() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        let actions = engine.on_ci_result("PROP-9999", true, "passed", &graph);
        assert!(actions.iter().any(|a| matches!(
            a,
            ConductorAction::LogDecision { message, .. }
            if message.contains("untracked")
        )));
    }

    #[test]
    fn test_ci_result_wrong_state_ignored() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        // Detect and pass CI → now in InReview
        detect_and_pass_ci(
            &mut engine,
            "PROP-2020",
            "EX-100: Test",
            "M5",
            Some("EX-100"),
            Some("expedition"),
            &graph,
        );

        // Second CI result should be ignored
        let actions = engine.on_ci_result("PROP-2020", true, "passed", &graph);
        assert!(actions.iter().any(|a| matches!(
            a,
            ConductorAction::LogDecision { message, .. }
            if message.contains("ignoring")
        )));
    }

    #[test]
    fn test_proposal_detected_avoids_self_review() {
        let mut engine = ReviewCycleEngine::new();
        let graph = graph_with_busy_m5();

        let actions = detect_and_pass_ci(
            &mut engine,
            "PROP-2021",
            "EX-100: Test",
            "M5",
            Some("EX-100"),
            Some("expedition"),
            &graph,
        );

        // Reviewer should NOT be M5
        let assign = actions
            .iter()
            .find(|a| matches!(a, ConductorAction::AssignReviewer { .. }));
        if let Some(ConductorAction::AssignReviewer { reviewer, .. }) = assign {
            assert_ne!(reviewer, "M5");
        }
    }

    #[test]
    fn test_review_approved_chore_auto_approves() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        detect_and_pass_ci(
            &mut engine,
            "PROP-2022",
            "CH-3020: Small fix",
            "Mini",
            Some("CH-3020"),
            Some("chore"),
            &graph,
        );

        let actions = engine.on_review_result("PROP-2022", true);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::AutoApprove { .. }))
        );

        let proposal = engine.tracked.get("PROP-2022").expect("tracked");
        assert_eq!(proposal.cycle_state, CycleState::Merged);
    }

    #[test]
    fn test_review_approved_expedition_escalates_to_captain() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        detect_and_pass_ci(
            &mut engine,
            "PROP-2023",
            "EX-3047: Conductor",
            "M5",
            Some("EX-3047"),
            Some("expedition"),
            &graph,
        );

        let actions = engine.on_review_result("PROP-2023", true);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::EscalateToCaptain { .. }))
        );

        let proposal = engine.tracked.get("PROP-2023").expect("tracked");
        assert_eq!(proposal.cycle_state, CycleState::EscalatedToCaptain);
    }

    #[test]
    fn test_changes_requested_routes_to_implementer() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        detect_and_pass_ci(
            &mut engine,
            "PROP-2024",
            "EX-100: Test",
            "DGX",
            Some("EX-100"),
            Some("expedition"),
            &graph,
        );

        let actions = engine.on_review_result("PROP-2024", false);
        assert!(actions.iter().any(|a| matches!(
            a,
            ConductorAction::RouteToImplementer { implementer, round, .. }
            if implementer == "DGX" && *round == 1
        )));

        let proposal = engine.tracked.get("PROP-2024").expect("tracked");
        assert_eq!(proposal.cycle_state, CycleState::ChangesRequested);
    }

    #[test]
    fn test_max_rounds_escalates() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        detect_and_pass_ci(
            &mut engine,
            "PROP-2025",
            "EX-100: Test",
            "DGX",
            Some("EX-100"),
            Some("expedition"),
            &graph,
        );

        // Round 1: changes requested
        engine.on_review_result("PROP-2025", false);
        // Fixes pushed → re-review
        engine.on_fixes_pushed("PROP-2025");
        // Round 2: changes requested again
        let actions = engine.on_review_result("PROP-2025", false);

        // Should escalate after 2 rounds
        assert!(actions.iter().any(
            |a| matches!(a, ConductorAction::EscalateToCaptain { reason, .. }
                if reason.contains("max review rounds"))
        ));

        let proposal = engine.tracked.get("PROP-2025").expect("tracked");
        assert_eq!(proposal.cycle_state, CycleState::EscalatedToCaptain);
        assert_eq!(proposal.review_rounds, 2);
    }

    #[test]
    fn test_fixes_pushed_triggers_rereview() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        detect_and_pass_ci(
            &mut engine,
            "PROP-2026",
            "CH-100: Fix",
            "Mini",
            Some("CH-100"),
            Some("chore"),
            &graph,
        );

        engine.on_review_result("PROP-2026", false);
        assert_eq!(
            engine.tracked["PROP-2026"].cycle_state,
            CycleState::ChangesRequested
        );

        let actions = engine.on_fixes_pushed("PROP-2026");
        assert_eq!(
            engine.tracked["PROP-2026"].cycle_state,
            CycleState::ReReview
        );
        // Should re-assign same reviewer
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::AssignReviewer { .. }))
        );
    }

    #[test]
    fn test_fixes_pushed_wrong_state_ignored() {
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        // Detect, pass CI → now AwaitingCi → InReview
        detect_and_pass_ci(
            &mut engine,
            "PROP-2027",
            "CH-100: Fix",
            "Mini",
            Some("CH-100"),
            Some("chore"),
            &graph,
        );

        // Still in InReview, not ChangesRequested
        let actions = engine.on_fixes_pushed("PROP-2027");
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::LogDecision { .. }))
        );
        assert_eq!(
            engine.tracked["PROP-2027"].cycle_state,
            CycleState::InReview
        );
    }

    #[test]
    fn test_review_result_untracked_proposal_logged() {
        let mut engine = ReviewCycleEngine::new();
        let actions = engine.on_review_result("PROP-9999", true);
        assert!(actions.iter().any(|a| matches!(
            a,
            ConductorAction::LogDecision { message, .. }
            if message.contains("untracked")
        )));
    }

    #[test]
    fn test_full_chore_lifecycle() {
        // End-to-end: detect → CI pass → assign reviewer → review approved → auto-approve
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        // 1. Detect → triggers CI
        let actions = engine.on_proposal_detected(
            "PROP-2028",
            "CH-3020: Add resolve",
            "main",
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

        // 2. CI passes → assigns reviewer
        let actions = engine.on_ci_result("PROP-2028", true, "all passed", &graph);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::AssignReviewer { .. }))
        );

        // 3. Reviewer approves
        let actions = engine.on_review_result("PROP-2028", true);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::AutoApprove { .. }))
        );

        // 4. Should be merged
        assert_eq!(engine.tracked["PROP-2028"].cycle_state, CycleState::Merged);
    }

    #[test]
    fn test_full_expedition_lifecycle_with_rejection() {
        // detect → CI pass → assign → changes requested → fix → re-review → approve → escalate
        let mut engine = ReviewCycleEngine::new();
        let graph = empty_graph();

        // 1. Detect + CI pass
        detect_and_pass_ci(
            &mut engine,
            "PROP-2029",
            "EX-3048: Review Cycle",
            "Mini",
            Some("EX-3048"),
            Some("expedition"),
            &graph,
        );

        // 2. Round 1: changes requested
        let actions = engine.on_review_result("PROP-2029", false);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::RouteToImplementer { .. }))
        );

        // 3. Fixes pushed
        engine.on_fixes_pushed("PROP-2029");

        // 4. Round 2: approved
        let actions = engine.on_review_result("PROP-2029", true);
        // Expedition → Captain required
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConductorAction::EscalateToCaptain { .. }))
        );
    }
}
