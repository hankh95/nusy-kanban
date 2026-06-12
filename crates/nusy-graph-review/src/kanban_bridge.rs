//! Kanban bridge — surface proposals as review items on the development board.
//!
//! Maps proposal state transitions to kanban item operations.
//! The bridge produces `KanbanAction` events that the consumer (e.g.,
//! nusy-kanban-server) translates into actual kanban operations.
//!
//! This avoids a circular dependency: nusy-graph-review does not depend on
//! nusy-kanban. Instead, it emits structured events that any kanban backend
//! can consume.

use crate::proposals::ProposalStatus;
use crate::safety_gates::ApprovalRequirement;

/// A kanban action to execute when a proposal state changes.
#[derive(Debug, Clone)]
pub enum KanbanAction {
    /// Create a new item on the development board.
    CreateItem {
        /// Item ID (e.g., "PROP-001").
        id: String,
        /// Title for the kanban item.
        title: String,
        /// Initial status.
        status: String,
        /// Assignee (the being/agent that created the proposal).
        assignee: String,
        /// Priority derived from safety classification.
        priority: String,
        /// Tags (includes proposal_type, y_layer, domain).
        tags: Vec<String>,
        /// Related items (e.g., the proposal ID).
        related: Vec<String>,
    },
    /// Update the status of an existing item.
    UpdateStatus { id: String, new_status: String },
    /// Move item to done with a final status note.
    Complete {
        id: String,
        outcome: String, // "merged", "rejected", "closed"
    },
}

/// Map a proposal ID to a kanban item ID.
pub fn proposal_to_kanban_id(proposal_id: &str) -> String {
    format!("PROP-{proposal_id}")
}

/// Map proposal status to kanban status.
pub fn proposal_status_to_kanban(status: &ProposalStatus) -> &'static str {
    match status {
        ProposalStatus::Draft => "backlog",
        ProposalStatus::Open => "in_progress",
        ProposalStatus::Reviewing => "in_progress",
        ProposalStatus::Approved => "review",
        ProposalStatus::Rejected => "done",
        ProposalStatus::Revised => "in_progress",
        ProposalStatus::Merged => "done",
        ProposalStatus::Closed => "done",
    }
}

/// Derive kanban priority from safety gate requirements.
pub fn safety_to_priority(requirement: &ApprovalRequirement) -> &'static str {
    if requirement.requires_human {
        "critical" // Y6 human gate
    } else if requirement.requires_shadow && requirement.auto_approve_threshold >= 0.05 {
        "high" // Safety-critical domain or Y2 reasoning
    } else if requirement.requires_shadow {
        "medium" // Shadow eval required
    } else {
        "low" // Auto-approvable
    }
}

/// Generate the kanban action for a newly created proposal.
pub fn on_proposal_created(
    proposal_id: &str,
    title: &str,
    author: &str,
    proposal_type: &str,
    requirement: &ApprovalRequirement,
) -> KanbanAction {
    let kanban_id = proposal_to_kanban_id(proposal_id);
    let priority = safety_to_priority(requirement);

    let mut tags = vec!["self-modification".to_string(), proposal_type.to_string()];
    if requirement.requires_human {
        tags.push("human-gate".to_string());
    }

    KanbanAction::CreateItem {
        id: kanban_id.clone(),
        title: format!("Self-Mod Proposal: {title}"),
        status: "backlog".to_string(),
        assignee: author.to_string(),
        priority: priority.to_string(),
        tags,
        related: vec![kanban_id],
    }
}

/// Generate the kanban action for a proposal state transition.
pub fn on_proposal_transition(proposal_id: &str, new_status: &ProposalStatus) -> KanbanAction {
    let kanban_id = proposal_to_kanban_id(proposal_id);
    let kanban_status = proposal_status_to_kanban(new_status);

    match new_status {
        ProposalStatus::Merged => KanbanAction::Complete {
            id: kanban_id,
            outcome: "merged".to_string(),
        },
        ProposalStatus::Rejected => KanbanAction::Complete {
            id: kanban_id,
            outcome: "rejected".to_string(),
        },
        ProposalStatus::Closed => KanbanAction::Complete {
            id: kanban_id,
            outcome: "closed".to_string(),
        },
        _ => KanbanAction::UpdateStatus {
            id: kanban_id,
            new_status: kanban_status.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety_gates::classify_change;
    use crate::safety_gates::default_gates;

    #[test]
    fn test_proposal_to_kanban_id() {
        assert_eq!(proposal_to_kanban_id("abc-123"), "PROP-abc-123");
    }

    #[test]
    fn test_status_mapping() {
        assert_eq!(proposal_status_to_kanban(&ProposalStatus::Draft), "backlog");
        assert_eq!(
            proposal_status_to_kanban(&ProposalStatus::Open),
            "in_progress"
        );
        assert_eq!(
            proposal_status_to_kanban(&ProposalStatus::Approved),
            "review"
        );
        assert_eq!(proposal_status_to_kanban(&ProposalStatus::Merged), "done");
        assert_eq!(proposal_status_to_kanban(&ProposalStatus::Rejected), "done");
        assert_eq!(proposal_status_to_kanban(&ProposalStatus::Closed), "done");
    }

    #[test]
    fn test_safety_priority_y6_critical() {
        let gates = default_gates().unwrap();
        let req = classify_change(&gates, 6, "general");
        assert_eq!(safety_to_priority(&req), "critical");
    }

    #[test]
    fn test_safety_priority_y2_high() {
        let gates = default_gates().unwrap();
        let req = classify_change(&gates, 2, "general");
        assert_eq!(safety_to_priority(&req), "high");
    }

    #[test]
    fn test_safety_priority_y5_medium() {
        let gates = default_gates().unwrap();
        let req = classify_change(&gates, 5, "engineering");
        assert_eq!(safety_to_priority(&req), "medium");
    }

    #[test]
    fn test_safety_priority_y0_low() {
        let gates = default_gates().unwrap();
        let req = classify_change(&gates, 0, "general");
        assert_eq!(safety_to_priority(&req), "low");
    }

    #[test]
    fn test_on_proposal_created() {
        let gates = default_gates().unwrap();
        let req = classify_change(&gates, 6, "general");
        let action = on_proposal_created(
            "001",
            "Fix calibration",
            "santiago",
            "knowledge_change",
            &req,
        );
        match action {
            KanbanAction::CreateItem { priority, tags, .. } => {
                assert_eq!(priority, "critical");
                assert!(tags.contains(&"human-gate".to_string()));
                assert!(tags.contains(&"self-modification".to_string()));
            }
            _ => panic!("Expected CreateItem"),
        }
    }

    #[test]
    fn test_on_proposal_merged() {
        let action = on_proposal_transition("001", &ProposalStatus::Merged);
        match action {
            KanbanAction::Complete { outcome, .. } => assert_eq!(outcome, "merged"),
            _ => panic!("Expected Complete"),
        }
    }

    #[test]
    fn test_on_proposal_rejected() {
        let action = on_proposal_transition("001", &ProposalStatus::Rejected);
        match action {
            KanbanAction::Complete { outcome, .. } => assert_eq!(outcome, "rejected"),
            _ => panic!("Expected Complete"),
        }
    }

    #[test]
    fn test_on_proposal_opened() {
        let action = on_proposal_transition("001", &ProposalStatus::Open);
        match action {
            KanbanAction::UpdateStatus { new_status, .. } => assert_eq!(new_status, "in_progress"),
            _ => panic!("Expected UpdateStatus"),
        }
    }
}
