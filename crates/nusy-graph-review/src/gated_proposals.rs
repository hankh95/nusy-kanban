//! Safety-gated proposal workflow — integrates SafetyGatesTable with ProposalStore.
//!
//! Wraps the proposal lifecycle with automatic safety classification:
//! - On open: classify all changed triples, attach requirements
//! - On approve: verify shadow eval results meet thresholds
//! - On merge: final gate check before merge proceeds
//!
//! This module bridges EXP-1285 (proposals) and EXP-1287 (safety gates).

use crate::proposals::{ProposalError, ProposalStore};
use crate::safety_gates::{
    ApprovalRequirement, ChangeEntry, SafetyGatesTable, classify_proposal_changes,
};

/// Errors from gated proposal operations.
#[derive(Debug, thiserror::Error)]
pub enum GatedProposalError {
    #[error("Proposal error: {0}")]
    Proposal(#[from] ProposalError),

    #[error("Safety gate requires human approval (gate: {gate_id})")]
    HumanGateRequired { gate_id: String },

    #[error("Shadow evaluation required but not provided")]
    ShadowEvalRequired,

    #[error("Shadow evaluation below threshold: improvement {actual:.4} < required {required:.4}")]
    BelowThreshold { actual: f64, required: f64 },

    #[error("Proof gates failed (CQ, KBDD, provenance, or do-calculus)")]
    ProofGatesFailed,
}

pub type Result<T> = std::result::Result<T, GatedProposalError>;

/// Shadow evaluation results provided by the caller.
#[derive(Debug, Clone)]
pub struct ShadowEvalResult {
    /// Metric improvement (0.0 = no change, 1.0 = 100% improvement).
    pub metric_improvement: f64,
    /// Whether the evaluation passed all proof gates (CQ, KBDD, provenance, etc.).
    pub proof_gates_passed: bool,
}

/// Safety metadata attached to a proposal after classification.
#[derive(Debug, Clone)]
pub struct ProposalSafetyMetadata {
    /// The computed approval requirement for this proposal.
    pub requirement: ApprovalRequirement,
    /// Whether human approval is needed (from the strictest gate).
    pub requires_human_approval: bool,
    /// Whether shadow evaluation is needed.
    pub requires_shadow_eval: bool,
    /// Shadow evaluation result (set after shadow eval completes).
    pub shadow_eval_passed: Option<bool>,
    /// Comma-separated list of triggered gate IDs.
    pub gate_ids: String,
}

/// Classify a proposal on open and return safety metadata.
///
/// Call this when a proposal transitions to `open`. The returned metadata
/// should be stored alongside the proposal (the caller is responsible for
/// attaching it to their tracking system).
pub fn classify_and_gate_proposal(
    gates: &SafetyGatesTable,
    _proposal_store: &ProposalStore,
    _proposal_id: &str,
    changes: &[ChangeEntry],
) -> ProposalSafetyMetadata {
    let req = classify_proposal_changes(gates, changes);
    ProposalSafetyMetadata {
        requires_human_approval: req.requires_human,
        requires_shadow_eval: req.requires_shadow,
        shadow_eval_passed: None,
        gate_ids: req.gate_id.clone(),
        requirement: req,
    }
}

/// Classify a proposal's changes against the safety gates.
///
/// Call this when a proposal transitions to `open`. Returns the most
/// restrictive approval requirement across all changed triples.
pub fn classify_proposal(gates: &SafetyGatesTable, changes: &[ChangeEntry]) -> ApprovalRequirement {
    classify_proposal_changes(gates, changes)
}

/// Check if a proposal can be approved given its safety requirements
/// and optional shadow evaluation results.
///
/// Returns Ok(()) if approval can proceed, or an error explaining why not.
pub fn check_approval_gate(
    requirement: &ApprovalRequirement,
    reviewer_is_human: bool,
    shadow_eval: Option<&ShadowEvalResult>,
) -> Result<()> {
    // 1. Human gate check
    if requirement.requires_human && !reviewer_is_human {
        return Err(GatedProposalError::HumanGateRequired {
            gate_id: requirement.gate_id.clone(),
        });
    }

    // 2. Shadow eval required check
    if requirement.requires_shadow {
        let eval = shadow_eval.ok_or(GatedProposalError::ShadowEvalRequired)?;

        // 3. Threshold check
        if eval.metric_improvement < requirement.auto_approve_threshold {
            return Err(GatedProposalError::BelowThreshold {
                actual: eval.metric_improvement,
                required: requirement.auto_approve_threshold,
            });
        }
    }

    Ok(())
}

/// Check if a proposal can be merged (final gate before merge).
///
/// Same checks as approval, plus verifies proof gates passed.
pub fn check_merge_gate(
    requirement: &ApprovalRequirement,
    shadow_eval: Option<&ShadowEvalResult>,
) -> Result<()> {
    // Shadow eval must exist for any non-trivial gate
    if requirement.requires_shadow {
        let eval = shadow_eval.ok_or(GatedProposalError::ShadowEvalRequired)?;

        if eval.metric_improvement < requirement.auto_approve_threshold {
            return Err(GatedProposalError::BelowThreshold {
                actual: eval.metric_improvement,
                required: requirement.auto_approve_threshold,
            });
        }

        // Proof gates (CQ, KBDD, provenance, do-calculus) must pass for merge
        if !eval.proof_gates_passed {
            return Err(GatedProposalError::ProofGatesFailed);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety_gates::default_gates;

    fn sample_y0_changes() -> Vec<ChangeEntry> {
        vec![ChangeEntry {
            y_layer: 0,
            domain: "general".to_string(),
        }]
    }

    fn sample_y6_changes() -> Vec<ChangeEntry> {
        vec![ChangeEntry {
            y_layer: 6,
            domain: "general".to_string(),
        }]
    }

    fn sample_y2_changes() -> Vec<ChangeEntry> {
        vec![ChangeEntry {
            y_layer: 2,
            domain: "general".to_string(),
        }]
    }

    fn sample_mixed_changes() -> Vec<ChangeEntry> {
        vec![
            ChangeEntry {
                y_layer: 0,
                domain: "general".to_string(),
            },
            ChangeEntry {
                y_layer: 6,
                domain: "general".to_string(),
            },
        ]
    }

    fn passing_shadow() -> ShadowEvalResult {
        ShadowEvalResult {
            metric_improvement: 0.15,
            proof_gates_passed: true,
        }
    }

    fn failing_shadow() -> ShadowEvalResult {
        ShadowEvalResult {
            metric_improvement: 0.01,
            proof_gates_passed: false,
        }
    }

    // ── Classify ──

    #[test]
    fn test_classify_y0_auto_approve() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y0_changes());
        assert!(!req.requires_human);
        assert!(!req.requires_shadow);
    }

    #[test]
    fn test_classify_y6_requires_human() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y6_changes());
        assert!(req.requires_human);
        assert!(req.requires_shadow);
    }

    #[test]
    fn test_classify_mixed_strictest_wins() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_mixed_changes());
        assert!(req.requires_human); // Y6 forces human
    }

    // ── Approval gate ──

    #[test]
    fn test_y0_auto_approves_without_shadow() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y0_changes());
        assert!(check_approval_gate(&req, false, None).is_ok());
    }

    #[test]
    fn test_y6_blocks_non_human_reviewer() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y6_changes());
        let result = check_approval_gate(&req, false, Some(&passing_shadow()));
        assert!(result.is_err());
        match result.unwrap_err() {
            GatedProposalError::HumanGateRequired { .. } => {}
            e => panic!("Expected HumanGateRequired, got: {e}"),
        }
    }

    #[test]
    fn test_y6_allows_human_reviewer_with_shadow() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y6_changes());
        assert!(check_approval_gate(&req, true, Some(&passing_shadow())).is_ok());
    }

    #[test]
    fn test_y2_requires_shadow() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y2_changes());
        let result = check_approval_gate(&req, false, None);
        assert!(result.is_err());
        match result.unwrap_err() {
            GatedProposalError::ShadowEvalRequired => {}
            e => panic!("Expected ShadowEvalRequired, got: {e}"),
        }
    }

    #[test]
    fn test_y2_with_passing_shadow_approves() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y2_changes());
        assert!(check_approval_gate(&req, false, Some(&passing_shadow())).is_ok());
    }

    #[test]
    fn test_below_threshold_rejects() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y2_changes());
        let result = check_approval_gate(&req, false, Some(&failing_shadow()));
        assert!(result.is_err());
        match result.unwrap_err() {
            GatedProposalError::BelowThreshold { actual, required } => {
                assert!(actual < required);
            }
            e => panic!("Expected BelowThreshold, got: {e}"),
        }
    }

    // ── Merge gate ──

    #[test]
    fn test_merge_gate_passes_with_shadow() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y2_changes());
        assert!(check_merge_gate(&req, Some(&passing_shadow())).is_ok());
    }

    #[test]
    fn test_merge_gate_blocks_without_shadow() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y2_changes());
        assert!(check_merge_gate(&req, None).is_err());
    }

    #[test]
    fn test_merge_gate_blocks_below_threshold() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y2_changes());
        assert!(check_merge_gate(&req, Some(&failing_shadow())).is_err());
    }

    #[test]
    fn test_merge_gate_blocks_when_proof_gates_failed() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y2_changes());
        // Good metric improvement but proof gates failed
        let eval = ShadowEvalResult {
            metric_improvement: 0.15,
            proof_gates_passed: false,
        };
        let result = check_merge_gate(&req, Some(&eval));
        assert!(result.is_err());
        match result.unwrap_err() {
            GatedProposalError::ProofGatesFailed => {}
            e => panic!("Expected ProofGatesFailed, got: {e}"),
        }
    }

    #[test]
    fn test_y0_merge_passes_without_shadow() {
        let gates = default_gates().unwrap();
        let req = classify_proposal(&gates, &sample_y0_changes());
        assert!(check_merge_gate(&req, None).is_ok());
    }
}
