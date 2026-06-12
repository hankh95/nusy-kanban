//! Experiment bridge — auto-generate EXPR items when proposals complete.
//!
//! When a self-modification proposal reaches a terminal state (merged,
//! rejected, closed), this module generates the data needed to create
//! an EXPR item on the research board.

/// The outcome of a self-modification experiment.
#[derive(Debug, Clone, PartialEq)]
pub enum ExperimentOutcome {
    /// Proposal was merged — the hypothesis was validated for this version.
    Validated,
    /// Proposal was rejected — the hypothesis was refuted for this version.
    Refuted,
    /// Proposal was closed without completing — withdrawn.
    Withdrawn,
}

impl ExperimentOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Validated => "VALIDATED",
            Self::Refuted => "REFUTED",
            Self::Withdrawn => "WITHDRAWN",
        }
    }
}

/// A metric comparison (before/after) from shadow evaluation.
#[derive(Debug, Clone)]
pub struct MetricComparison {
    pub name: String,
    pub baseline: f64,
    pub shadow: f64,
    pub delta: f64,
}

/// All data needed to create an EXPR item on the research board.
#[derive(Debug, Clone)]
pub struct ExperimentRecord {
    /// Auto-generated experiment ID (e.g., "EXPR-auto-001").
    pub experiment_id: String,
    /// Title: "Self-Modification: {proposal_title}".
    pub title: String,
    /// The being that ran the experiment.
    pub being_name: String,
    /// What the being expected to improve.
    pub hypothesis_description: String,
    /// Y-layer of the changes.
    pub y_layer: u8,
    /// Domain of the changes.
    pub domain: String,
    /// Summary of what was changed.
    pub diff_summary: String,
    /// Safety gate that was applied.
    pub gate_description: String,
    /// Metric comparisons (before/after).
    pub metrics: Vec<MetricComparison>,
    /// Outcome of the experiment.
    pub outcome: ExperimentOutcome,
    /// Rationale for the outcome.
    pub rationale: String,
    /// Related proposal ID.
    pub proposal_id: String,
    /// Tags for the EXPR item.
    pub tags: Vec<String>,
}

/// Generate an ExperimentRecord from a completed proposal.
#[allow(clippy::too_many_arguments)]
pub fn generate_experiment_record(
    proposal_id: &str,
    proposal_title: &str,
    being_name: &str,
    hypothesis: &str,
    y_layer: u8,
    domain: &str,
    diff_summary: &str,
    gate_description: &str,
    metrics: Vec<MetricComparison>,
    outcome: ExperimentOutcome,
    rationale: &str,
    auto_id_counter: u32,
) -> ExperimentRecord {
    ExperimentRecord {
        experiment_id: format!("EXPR-auto-{auto_id_counter:03}"),
        title: format!("Self-Modification: {proposal_title}"),
        being_name: being_name.to_string(),
        hypothesis_description: hypothesis.to_string(),
        y_layer,
        domain: domain.to_string(),
        diff_summary: diff_summary.to_string(),
        gate_description: gate_description.to_string(),
        metrics,
        outcome: outcome.clone(),
        rationale: rationale.to_string(),
        proposal_id: proposal_id.to_string(),
        tags: vec![
            "self-modification".to_string(),
            format!("y{y_layer}"),
            domain.to_string(),
        ],
    }
}

/// Render an ExperimentRecord as markdown (for EXPR file creation).
pub fn render_experiment_markdown(record: &ExperimentRecord) -> String {
    let mut md = String::new();

    // Frontmatter
    md.push_str("---\n");
    md.push_str(&format!("id: {}\n", record.experiment_id));
    md.push_str(&format!("title: \"{}\"\n", record.title));
    md.push_str("type: experiment\n");
    md.push_str("status: complete\n");
    md.push_str(&format!("tags: [{}]\n", record.tags.join(", ")));
    md.push_str(&format!("related: [PROP-{}]\n", record.proposal_id));
    md.push_str("---\n\n");

    // Body
    md.push_str(&format!("# {}: {}\n\n", record.experiment_id, record.title));

    md.push_str("## Hypothesis\n");
    md.push_str(&format!(
        "{} hypothesized that {} would improve performance.\n\n",
        record.being_name, record.hypothesis_description
    ));

    md.push_str("## Method\n");
    md.push_str(&format!("- **Y-Layer:** Y{}\n", record.y_layer));
    md.push_str(&format!("- **Domain:** {}\n", record.domain));
    md.push_str(&format!("- **Changes:** {}\n", record.diff_summary));
    md.push_str(&format!(
        "- **Safety classification:** {}\n\n",
        record.gate_description
    ));

    md.push_str("## Results\n");
    md.push_str("| Metric | Baseline | Shadow | Delta |\n");
    md.push_str("|--------|----------|--------|-------|\n");
    for m in &record.metrics {
        md.push_str(&format!(
            "| {} | {:.4} | {:.4} | {:+.4} |\n",
            m.name, m.baseline, m.shadow, m.delta
        ));
    }
    md.push('\n');

    md.push_str("## Conclusion\n");
    md.push_str(&format!(
        "**{}** — {}\n",
        record.outcome.as_str(),
        record.rationale
    ));

    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> ExperimentRecord {
        generate_experiment_record(
            "abc-123",
            "Improve entity grounding",
            "santiago-developer-v14.3",
            "adding clinical entity triples",
            1,
            "medical",
            "Added 42 triples to Y1 medical namespace",
            "Y1 medical: shadow eval + do-calculus gate",
            vec![
                MetricComparison {
                    name: "entity_accuracy".to_string(),
                    baseline: 0.72,
                    shadow: 0.81,
                    delta: 0.09,
                },
                MetricComparison {
                    name: "provenance_integrity".to_string(),
                    baseline: 0.999,
                    shadow: 0.999,
                    delta: 0.0,
                },
            ],
            ExperimentOutcome::Validated,
            "Entity accuracy improved 9% with no provenance regression",
            1,
        )
    }

    #[test]
    fn test_generate_experiment_record() {
        let record = sample_record();
        assert_eq!(record.experiment_id, "EXPR-auto-001");
        assert_eq!(record.title, "Self-Modification: Improve entity grounding");
        assert_eq!(record.outcome, ExperimentOutcome::Validated);
        assert_eq!(record.metrics.len(), 2);
        assert!(record.tags.contains(&"self-modification".to_string()));
        assert!(record.tags.contains(&"y1".to_string()));
        assert!(record.tags.contains(&"medical".to_string()));
    }

    #[test]
    fn test_render_markdown_contains_frontmatter() {
        let record = sample_record();
        let md = render_experiment_markdown(&record);
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: EXPR-auto-001"));
        assert!(md.contains("type: experiment"));
        assert!(md.contains("status: complete"));
        assert!(md.contains("related: [PROP-abc-123]"));
    }

    #[test]
    fn test_render_markdown_contains_results_table() {
        let record = sample_record();
        let md = render_experiment_markdown(&record);
        assert!(md.contains("| entity_accuracy |"));
        assert!(md.contains("| provenance_integrity |"));
        assert!(md.contains("+0.0900"));
    }

    #[test]
    fn test_render_markdown_contains_conclusion() {
        let record = sample_record();
        let md = render_experiment_markdown(&record);
        assert!(md.contains("**VALIDATED**"));
        assert!(md.contains("Entity accuracy improved 9%"));
    }

    #[test]
    fn test_refuted_outcome() {
        let record = generate_experiment_record(
            "def-456",
            "Change reasoning rules",
            "santiago",
            "modifying inference thresholds",
            2,
            "general",
            "Modified 3 Y2 rules",
            "Y2 default: do-calculus gate",
            vec![MetricComparison {
                name: "cq_coverage".to_string(),
                baseline: 0.85,
                shadow: 0.78,
                delta: -0.07,
            }],
            ExperimentOutcome::Refuted,
            "CQ coverage regressed 7%",
            2,
        );
        assert_eq!(record.outcome.as_str(), "REFUTED");
        let md = render_experiment_markdown(&record);
        assert!(md.contains("**REFUTED**"));
    }

    #[test]
    fn test_withdrawn_outcome() {
        let record = generate_experiment_record(
            "ghi-789",
            "Aborted change",
            "santiago",
            "testing",
            0,
            "general",
            "N/A",
            "N/A",
            vec![],
            ExperimentOutcome::Withdrawn,
            "Closed by author",
            3,
        );
        assert_eq!(record.outcome.as_str(), "WITHDRAWN");
    }
}
