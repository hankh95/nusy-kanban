//! HDD loop closure — hypothesis evidence accumulation and cross-board links.
//!
//! Completes the feedback loop: experiment results update hypothesis evidence
//! and maintain RDF triples linking proposals, experiments, and hypotheses
//! across both boards.

use crate::experiment_bridge::ExperimentOutcome;

/// An RDF triple representing a cross-board link.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossBoardTriple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

/// Generate the cross-board link triples for a completed experiment.
///
/// Creates three link types:
/// 1. proposal → producedExperiment → experiment
/// 2. experiment → testedHypothesis → hypothesis (if provided)
/// 3. hypothesis → evidencedBy → experiment (if provided)
pub fn generate_cross_board_links(
    proposal_id: &str,
    experiment_id: &str,
    hypothesis_id: Option<&str>,
) -> Vec<CrossBoardTriple> {
    let mut triples = vec![CrossBoardTriple {
        subject: format!("PROP-{proposal_id}"),
        predicate: "kb:producedExperiment".to_string(),
        object: experiment_id.to_string(),
    }];

    if let Some(hyp_id) = hypothesis_id {
        triples.push(CrossBoardTriple {
            subject: experiment_id.to_string(),
            predicate: "kb:testedHypothesis".to_string(),
            object: hyp_id.to_string(),
        });
        triples.push(CrossBoardTriple {
            subject: hyp_id.to_string(),
            predicate: "kb:evidencedBy".to_string(),
            object: experiment_id.to_string(),
        });
    }

    triples
}

/// Evidence summary for a hypothesis across multiple experiments.
#[derive(Debug, Clone)]
pub struct HypothesisEvidence {
    /// Hypothesis ID.
    pub hypothesis_id: String,
    /// Total experiments that tested this hypothesis.
    pub total_experiments: usize,
    /// Number that validated.
    pub validated_count: usize,
    /// Number that refuted.
    pub refuted_count: usize,
    /// Number withdrawn.
    pub withdrawn_count: usize,
    /// Experiment IDs with outcomes.
    pub experiments: Vec<(String, ExperimentOutcome)>,
}

impl HypothesisEvidence {
    /// Create a new evidence tracker for a hypothesis.
    pub fn new(hypothesis_id: &str) -> Self {
        Self {
            hypothesis_id: hypothesis_id.to_string(),
            total_experiments: 0,
            validated_count: 0,
            refuted_count: 0,
            withdrawn_count: 0,
            experiments: Vec::new(),
        }
    }

    /// Record an experiment outcome.
    pub fn record(&mut self, experiment_id: &str, outcome: ExperimentOutcome) {
        self.total_experiments += 1;
        match &outcome {
            ExperimentOutcome::Validated => self.validated_count += 1,
            ExperimentOutcome::Refuted => self.refuted_count += 1,
            ExperimentOutcome::Withdrawn => self.withdrawn_count += 1,
        }
        self.experiments.push((experiment_id.to_string(), outcome));
    }

    /// Summary string: "2/3 experiments validated".
    pub fn summary(&self) -> String {
        let active = self.total_experiments - self.withdrawn_count;
        if active == 0 {
            "No experiments completed".to_string()
        } else {
            format!("{}/{} experiments validated", self.validated_count, active)
        }
    }

    /// Whether the hypothesis has strong evidence (majority validated).
    pub fn has_strong_evidence(&self) -> bool {
        let active = self.total_experiments - self.withdrawn_count;
        active > 0 && self.validated_count > active / 2
    }
}

/// Accumulate evidence across experiments for a hypothesis.
pub fn accumulate_evidence(
    hypothesis_id: &str,
    experiments: &[(String, ExperimentOutcome)],
) -> HypothesisEvidence {
    let mut evidence = HypothesisEvidence::new(hypothesis_id);
    for (exp_id, outcome) in experiments {
        evidence.record(exp_id, outcome.clone());
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_links_with_hypothesis() {
        let links = generate_cross_board_links("001", "EXPR-auto-001", Some("H-042"));
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].subject, "PROP-001");
        assert_eq!(links[0].predicate, "kb:producedExperiment");
        assert_eq!(links[0].object, "EXPR-auto-001");
        assert_eq!(links[1].predicate, "kb:testedHypothesis");
        assert_eq!(links[2].predicate, "kb:evidencedBy");
        assert_eq!(links[2].subject, "H-042");
    }

    #[test]
    fn test_generate_links_without_hypothesis() {
        let links = generate_cross_board_links("001", "EXPR-auto-001", None);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].predicate, "kb:producedExperiment");
    }

    #[test]
    fn test_evidence_accumulation() {
        let experiments = vec![
            ("EXPR-1".to_string(), ExperimentOutcome::Validated),
            ("EXPR-2".to_string(), ExperimentOutcome::Refuted),
            ("EXPR-3".to_string(), ExperimentOutcome::Validated),
        ];
        let evidence = accumulate_evidence("H-042", &experiments);
        assert_eq!(evidence.total_experiments, 3);
        assert_eq!(evidence.validated_count, 2);
        assert_eq!(evidence.refuted_count, 1);
        assert_eq!(evidence.summary(), "2/3 experiments validated");
        assert!(evidence.has_strong_evidence());
    }

    #[test]
    fn test_evidence_no_strong_evidence() {
        let experiments = vec![
            ("EXPR-1".to_string(), ExperimentOutcome::Refuted),
            ("EXPR-2".to_string(), ExperimentOutcome::Refuted),
        ];
        let evidence = accumulate_evidence("H-100", &experiments);
        assert!(!evidence.has_strong_evidence());
        assert_eq!(evidence.summary(), "0/2 experiments validated");
    }

    #[test]
    fn test_evidence_withdrawn_excluded_from_active() {
        let experiments = vec![
            ("EXPR-1".to_string(), ExperimentOutcome::Validated),
            ("EXPR-2".to_string(), ExperimentOutcome::Withdrawn),
        ];
        let evidence = accumulate_evidence("H-050", &experiments);
        assert_eq!(evidence.summary(), "1/1 experiments validated");
        assert!(evidence.has_strong_evidence());
    }

    #[test]
    fn test_evidence_all_withdrawn() {
        let experiments = vec![("EXPR-1".to_string(), ExperimentOutcome::Withdrawn)];
        let evidence = accumulate_evidence("H-099", &experiments);
        assert_eq!(evidence.summary(), "No experiments completed");
        assert!(!evidence.has_strong_evidence());
    }
}
