//! Safety gates — Y-layer × domain approval requirements.
//!
//! Encodes the approval matrix from EXP-1286 (Self-Modification Protocol):
//! - Which changes auto-approve, which require shadow eval, which need human gate
//! - Domain overrides (medical, legal, financial → stricter gates)
//! - Y6 metacognition is ALWAYS human-gated (quis custodiet)
//!
//! The approval engine is domain-agnostic. Proof infrastructure (CQ coverage,
//! KBDD yields, do-calculus, provenance, SchemaMatch) is the universal gate.
//! Human gates exist only for Y6 self-modification of the evaluator.

use arrow::array::{Array, BooleanArray, Float64Array, RecordBatch, StringArray, UInt8Array};
use arrow::datatypes::{DataType, Field, Schema};
use std::sync::Arc;

// Re-export YLayer from nusy-arrow-core to avoid duplicate enum.
pub use nusy_arrow_core::YLayer;

/// Errors from safety gate operations.
#[derive(Debug, thiserror::Error)]
pub enum SafetyGateError {
    #[error("Invalid Y-layer: {0}")]
    InvalidYLayer(u8),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

pub type Result<T> = std::result::Result<T, SafetyGateError>;

/// The result of classifying a change against the safety matrix.
#[derive(Debug, Clone)]
pub struct ApprovalRequirement {
    /// Whether human approval is required.
    pub requires_human: bool,
    /// Whether shadow evaluation must pass before approval.
    pub requires_shadow: bool,
    /// Minimum metric improvement threshold for auto-approval (0.0-1.0).
    /// Changes must improve metrics by at least this amount.
    pub auto_approve_threshold: f64,
    /// ID of the gate rule that matched.
    pub gate_id: String,
    /// Human-readable explanation.
    pub description: String,
}

/// Wildcard domain — matches any domain not explicitly configured.
pub const WILDCARD_DOMAIN: &str = "*";

/// The safety gates table — an Arrow RecordBatch encoding approval rules.
#[derive(Debug, Clone)]
pub struct SafetyGatesTable {
    batch: RecordBatch,
}

/// Column indices for SafetyGatesTable.
mod col {
    pub const GATE_ID: usize = 0;
    pub const Y_LAYER: usize = 1;
    pub const DOMAIN: usize = 2;
    pub const REQUIRES_HUMAN: usize = 3;
    pub const REQUIRES_SHADOW: usize = 4;
    pub const AUTO_APPROVE_THRESHOLD: usize = 5;
    pub const DESCRIPTION: usize = 6;
}

/// Arrow schema for the safety gates table.
pub fn safety_gates_schema() -> Schema {
    Schema::new(vec![
        Field::new("gate_id", DataType::Utf8, false),
        Field::new("y_layer", DataType::UInt8, false),
        Field::new("domain", DataType::Utf8, false),
        Field::new("requires_human", DataType::Boolean, false),
        Field::new("requires_shadow", DataType::Boolean, false),
        Field::new("auto_approve_threshold", DataType::Float64, false),
        Field::new("description", DataType::Utf8, false),
    ])
}

/// A single gate rule for building the table.
struct GateRule {
    gate_id: &'static str,
    y_layer: u8,
    domain: &'static str,
    requires_human: bool,
    requires_shadow: bool,
    threshold: f64,
    description: &'static str,
}

/// Build the default safety gates configuration from EXP-1286 protocol.
///
/// Captain decision (2026-03-15): Do-calculus proof gates replace human gates
/// for Y0-Y5 in all domains. Human gate exists ONLY for Y6 metacognition.
/// The `requires_human` field for safety-critical domains is kept as metadata
/// for audit/observability but the actual gate is the proof stack.
pub fn default_gates() -> Result<SafetyGatesTable> {
    let rules = vec![
        // Y0: Prose — auto-approve if proofs pass
        GateRule {
            gate_id: "y0-default",
            y_layer: 0,
            domain: WILDCARD_DOMAIN,
            requires_human: false,
            requires_shadow: false,
            threshold: 0.0,
            description: "Y0 prose: auto-approve if metrics don't regress",
        },
        GateRule {
            gate_id: "y0-medical",
            y_layer: 0,
            domain: "medical",
            requires_human: false,
            requires_shadow: true,
            threshold: 0.05,
            description: "Y0 medical prose: shadow eval required, do-calculus gate",
        },
        GateRule {
            gate_id: "y0-legal",
            y_layer: 0,
            domain: "legal",
            requires_human: false,
            requires_shadow: true,
            threshold: 0.05,
            description: "Y0 legal prose: shadow eval required, do-calculus gate",
        },
        // Y1: Semantic — auto-approve if proofs pass
        GateRule {
            gate_id: "y1-default",
            y_layer: 1,
            domain: WILDCARD_DOMAIN,
            requires_human: false,
            requires_shadow: false,
            threshold: 0.0,
            description: "Y1 semantic: auto-approve if metrics don't regress",
        },
        GateRule {
            gate_id: "y1-medical",
            y_layer: 1,
            domain: "medical",
            requires_human: false,
            requires_shadow: true,
            threshold: 0.05,
            description: "Y1 medical relationships: shadow eval + do-calculus gate",
        },
        // Y2: Reasoning — do-calculus gate required (causal proof)
        GateRule {
            gate_id: "y2-default",
            y_layer: 2,
            domain: WILDCARD_DOMAIN,
            requires_human: false,
            requires_shadow: true,
            threshold: 0.05,
            description: "Y2 reasoning rules: do-calculus causal proof required",
        },
        // Y3: Experience — being's own, always auto-approve
        GateRule {
            gate_id: "y3-default",
            y_layer: 3,
            domain: WILDCARD_DOMAIN,
            requires_human: false,
            requires_shadow: false,
            threshold: 0.0,
            description: "Y3 experience: being's own history, auto-approve",
        },
        // Y4: Journal — being's own, always auto-approve
        GateRule {
            gate_id: "y4-default",
            y_layer: 4,
            domain: WILDCARD_DOMAIN,
            requires_human: false,
            requires_shadow: false,
            threshold: 0.0,
            description: "Y4 journal: being's own thoughts, auto-approve",
        },
        // Y5: Procedural — shadow eval required
        GateRule {
            gate_id: "y5-default",
            y_layer: 5,
            domain: WILDCARD_DOMAIN,
            requires_human: false,
            requires_shadow: true,
            threshold: 0.02,
            description: "Y5 procedures: shadow eval required, do-calculus gate",
        },
        GateRule {
            gate_id: "y5-medical",
            y_layer: 5,
            domain: "medical",
            requires_human: false,
            requires_shadow: true,
            threshold: 0.05,
            description: "Y5 clinical procedures: shadow eval + strict threshold",
        },
        GateRule {
            gate_id: "y5-legal",
            y_layer: 5,
            domain: "legal",
            requires_human: false,
            requires_shadow: true,
            threshold: 0.05,
            description: "Y5 legal procedures: shadow eval + strict threshold",
        },
        // Y6: Metacognition — ALWAYS human gate (quis custodiet)
        GateRule {
            gate_id: "y6-default",
            y_layer: 6,
            domain: WILDCARD_DOMAIN,
            requires_human: true,
            requires_shadow: true,
            threshold: 0.10,
            description: "Y6 metacognition: ALWAYS human gate — evaluator cannot evaluate itself",
        },
    ];

    SafetyGatesTable::from_rules(&rules)
}

impl SafetyGatesTable {
    /// Build from a list of gate rules.
    fn from_rules(rules: &[GateRule]) -> Result<Self> {
        let gate_ids: Vec<&str> = rules.iter().map(|r| r.gate_id).collect();
        let y_layers: Vec<u8> = rules.iter().map(|r| r.y_layer).collect();
        let domains: Vec<&str> = rules.iter().map(|r| r.domain).collect();
        let humans: Vec<bool> = rules.iter().map(|r| r.requires_human).collect();
        let shadows: Vec<bool> = rules.iter().map(|r| r.requires_shadow).collect();
        let thresholds: Vec<f64> = rules.iter().map(|r| r.threshold).collect();
        let descriptions: Vec<&str> = rules.iter().map(|r| r.description).collect();

        let schema = Arc::new(safety_gates_schema());
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(gate_ids)),
                Arc::new(UInt8Array::from(y_layers)),
                Arc::new(StringArray::from(domains)),
                Arc::new(BooleanArray::from(humans)),
                Arc::new(BooleanArray::from(shadows)),
                Arc::new(Float64Array::from(thresholds)),
                Arc::new(StringArray::from(descriptions)),
            ],
        )?;

        Ok(Self { batch })
    }

    /// Number of gate rules.
    pub fn len(&self) -> usize {
        self.batch.num_rows()
    }

    /// Whether the table has no rules.
    pub fn is_empty(&self) -> bool {
        self.batch.num_rows() == 0
    }

    /// Get the underlying RecordBatch.
    pub fn batch(&self) -> &RecordBatch {
        &self.batch
    }

    fn gate_ids(&self) -> &StringArray {
        self.batch
            .column(col::GATE_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("gate_id column")
    }

    fn y_layers(&self) -> &UInt8Array {
        self.batch
            .column(col::Y_LAYER)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .expect("y_layer column")
    }

    fn domains(&self) -> &StringArray {
        self.batch
            .column(col::DOMAIN)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("domain column")
    }

    fn requires_human(&self) -> &BooleanArray {
        self.batch
            .column(col::REQUIRES_HUMAN)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("requires_human column")
    }

    fn requires_shadow(&self) -> &BooleanArray {
        self.batch
            .column(col::REQUIRES_SHADOW)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("requires_shadow column")
    }

    fn thresholds(&self) -> &Float64Array {
        self.batch
            .column(col::AUTO_APPROVE_THRESHOLD)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("threshold column")
    }

    fn descriptions(&self) -> &StringArray {
        self.batch
            .column(col::DESCRIPTION)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("description column")
    }
}

/// Classify a single change by Y-layer and domain.
///
/// Lookup order:
/// 1. Exact match (y_layer, domain)
/// 2. Wildcard match (y_layer, "*")
/// 3. Most restrictive default (human + shadow + 0.10)
pub fn classify_change(gates: &SafetyGatesTable, y_layer: u8, domain: &str) -> ApprovalRequirement {
    let y_layers = gates.y_layers();
    let domains = gates.domains();
    let gate_ids = gates.gate_ids();
    let humans = gates.requires_human();
    let shadows = gates.requires_shadow();
    let thresholds = gates.thresholds();
    let descriptions = gates.descriptions();

    // 1. Exact match
    for i in 0..gates.len() {
        if y_layers.value(i) == y_layer && domains.value(i) == domain {
            return ApprovalRequirement {
                requires_human: humans.value(i),
                requires_shadow: shadows.value(i),
                auto_approve_threshold: thresholds.value(i),
                gate_id: gate_ids.value(i).to_string(),
                description: descriptions.value(i).to_string(),
            };
        }
    }

    // 2. Wildcard match
    for i in 0..gates.len() {
        if y_layers.value(i) == y_layer && domains.value(i) == WILDCARD_DOMAIN {
            return ApprovalRequirement {
                requires_human: humans.value(i),
                requires_shadow: shadows.value(i),
                auto_approve_threshold: thresholds.value(i),
                gate_id: gate_ids.value(i).to_string(),
                description: descriptions.value(i).to_string(),
            };
        }
    }

    // 3. No match — most restrictive default (human + shadow + 0.10)
    ApprovalRequirement {
        requires_human: true,
        requires_shadow: true,
        auto_approve_threshold: 0.10,
        gate_id: "default-restrictive".to_string(),
        description: "No matching gate — defaulting to most restrictive".to_string(),
    }
}

/// A changed triple with its Y-layer and domain for batch classification.
#[derive(Debug, Clone)]
pub struct ChangeEntry {
    pub y_layer: u8,
    pub domain: String,
}

/// Classify a batch of changes (e.g., from a proposal diff).
///
/// Returns the **most restrictive** requirement across all changes:
/// - If ANY change requires human approval → whole proposal requires human
/// - If ANY change requires shadow eval → whole proposal requires shadow
/// - Threshold = maximum threshold across all changes
pub fn classify_proposal_changes(
    gates: &SafetyGatesTable,
    changes: &[ChangeEntry],
) -> ApprovalRequirement {
    if changes.is_empty() {
        return ApprovalRequirement {
            requires_human: false,
            requires_shadow: false,
            auto_approve_threshold: 0.0,
            gate_id: "empty-proposal".to_string(),
            description: "No changes in proposal".to_string(),
        };
    }

    let mut requires_human = false;
    let mut requires_shadow = false;
    let mut max_threshold: f64 = 0.0;
    let mut strictest_gate = String::new();
    let mut strictest_desc = String::new();

    for change in changes {
        let req = classify_change(gates, change.y_layer, &change.domain);

        if req.requires_human {
            requires_human = true;
        }
        if req.requires_shadow {
            requires_shadow = true;
        }
        if req.auto_approve_threshold > max_threshold {
            max_threshold = req.auto_approve_threshold;
            strictest_gate = req.gate_id;
            strictest_desc = req.description;
        }
    }

    ApprovalRequirement {
        requires_human,
        requires_shadow,
        auto_approve_threshold: max_threshold,
        gate_id: strictest_gate,
        description: strictest_desc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gates() -> SafetyGatesTable {
        default_gates().expect("default gates")
    }

    // ── Y0: Prose ──

    #[test]
    fn test_y0_default_auto_approves() {
        let g = gates();
        let req = classify_change(&g, 0, "general");
        assert!(!req.requires_human);
        assert!(!req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.0);
        assert_eq!(req.gate_id, "y0-default");
    }

    #[test]
    fn test_y0_medical_requires_shadow() {
        let g = gates();
        let req = classify_change(&g, 0, "medical");
        assert!(!req.requires_human); // Captain: do-calculus, not human
        assert!(req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.05);
        assert_eq!(req.gate_id, "y0-medical");
    }

    #[test]
    fn test_y0_legal_requires_shadow() {
        let g = gates();
        let req = classify_change(&g, 0, "legal");
        assert!(!req.requires_human);
        assert!(req.requires_shadow);
        assert_eq!(req.gate_id, "y0-legal");
    }

    // ── Y1: Semantic ──

    #[test]
    fn test_y1_default_auto_approves() {
        let g = gates();
        let req = classify_change(&g, 1, "engineering");
        assert!(!req.requires_human);
        assert!(!req.requires_shadow);
        assert_eq!(req.gate_id, "y1-default");
    }

    #[test]
    fn test_y1_medical_requires_shadow() {
        let g = gates();
        let req = classify_change(&g, 1, "medical");
        assert!(!req.requires_human);
        assert!(req.requires_shadow);
        assert_eq!(req.gate_id, "y1-medical");
    }

    // ── Y2: Reasoning ──

    #[test]
    fn test_y2_always_requires_shadow() {
        let g = gates();
        let req = classify_change(&g, 2, "general");
        assert!(!req.requires_human); // Captain: do-calculus, not human
        assert!(req.requires_shadow);
        assert!(req.auto_approve_threshold >= 0.05);
        assert_eq!(req.gate_id, "y2-default");
    }

    #[test]
    fn test_y2_any_domain_same_gate() {
        let g = gates();
        // Y2 has no domain overrides — always uses default
        let req_general = classify_change(&g, 2, "general");
        let req_medical = classify_change(&g, 2, "medical");
        assert_eq!(req_general.gate_id, req_medical.gate_id);
    }

    // ── Y3: Experience ──

    #[test]
    fn test_y3_always_auto_approves() {
        let g = gates();
        let req = classify_change(&g, 3, "medical");
        assert!(!req.requires_human);
        assert!(!req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.0);
        assert_eq!(req.gate_id, "y3-default");
    }

    // ── Y4: Journal ──

    #[test]
    fn test_y4_always_auto_approves() {
        let g = gates();
        let req = classify_change(&g, 4, "legal");
        assert!(!req.requires_human);
        assert!(!req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.0);
        assert_eq!(req.gate_id, "y4-default");
    }

    // ── Y5: Procedural ──

    #[test]
    fn test_y5_default_requires_shadow() {
        let g = gates();
        let req = classify_change(&g, 5, "engineering");
        assert!(!req.requires_human);
        assert!(req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.02);
        assert_eq!(req.gate_id, "y5-default");
    }

    #[test]
    fn test_y5_medical_stricter_threshold() {
        let g = gates();
        let req = classify_change(&g, 5, "medical");
        assert!(!req.requires_human); // Captain: do-calculus, not human
        assert!(req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.05);
        assert_eq!(req.gate_id, "y5-medical");
    }

    #[test]
    fn test_y5_legal_stricter_threshold() {
        let g = gates();
        let req = classify_change(&g, 5, "legal");
        assert!(req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.05);
        assert_eq!(req.gate_id, "y5-legal");
    }

    // ── Y6: Metacognition ──

    #[test]
    fn test_y6_always_requires_human() {
        let g = gates();
        let req = classify_change(&g, 6, "general");
        assert!(req.requires_human); // The ONLY human gate
        assert!(req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.10);
        assert_eq!(req.gate_id, "y6-default");
    }

    #[test]
    fn test_y6_any_domain_always_human() {
        let g = gates();
        for domain in &["general", "medical", "legal", "financial", "infrastructure"] {
            let req = classify_change(&g, 6, domain);
            assert!(req.requires_human, "Y6 {domain} should require human");
            assert!(req.requires_shadow, "Y6 {domain} should require shadow");
        }
    }

    // ── Edge cases ──

    #[test]
    fn test_unknown_domain_uses_wildcard() {
        let g = gates();
        let req = classify_change(&g, 0, "underwater_basket_weaving");
        assert_eq!(req.gate_id, "y0-default"); // falls back to wildcard
        assert!(!req.requires_human);
    }

    #[test]
    fn test_no_matching_gate_uses_restrictive_default() {
        // Create an empty gates table
        let empty = SafetyGatesTable::from_rules(&[]).expect("empty gates");
        let req = classify_change(&empty, 0, "general");
        assert!(req.requires_human);
        assert!(req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.10);
        assert_eq!(req.gate_id, "default-restrictive");
    }

    #[test]
    fn test_invalid_y_layer_uses_restrictive_default() {
        let g = gates();
        let req = classify_change(&g, 99, "general");
        assert!(req.requires_human);
        assert!(req.requires_shadow);
        assert_eq!(req.gate_id, "default-restrictive");
    }

    // ── Batch classification ──

    #[test]
    fn test_classify_empty_proposal() {
        let g = gates();
        let req = classify_proposal_changes(&g, &[]);
        assert!(!req.requires_human);
        assert!(!req.requires_shadow);
        assert_eq!(req.gate_id, "empty-proposal");
    }

    #[test]
    fn test_classify_single_change() {
        let g = gates();
        let changes = vec![ChangeEntry {
            y_layer: 0,
            domain: "general".to_string(),
        }];
        let req = classify_proposal_changes(&g, &changes);
        assert!(!req.requires_human);
        assert!(!req.requires_shadow);
    }

    #[test]
    fn test_mixed_changes_use_strictest_gate() {
        let g = gates();
        let changes = vec![
            ChangeEntry {
                y_layer: 0,
                domain: "general".to_string(),
            }, // auto-approve
            ChangeEntry {
                y_layer: 2,
                domain: "general".to_string(),
            }, // shadow required
        ];
        let req = classify_proposal_changes(&g, &changes);
        assert!(!req.requires_human); // Y2 doesn't require human
        assert!(req.requires_shadow); // Y2 requires shadow — strictest wins
        assert!(req.auto_approve_threshold >= 0.05);
    }

    #[test]
    fn test_y6_in_batch_forces_human_gate() {
        let g = gates();
        let changes = vec![
            ChangeEntry {
                y_layer: 3,
                domain: "general".to_string(),
            }, // Y3: auto-approve
            ChangeEntry {
                y_layer: 6,
                domain: "general".to_string(),
            }, // Y6: human gate
        ];
        let req = classify_proposal_changes(&g, &changes);
        assert!(req.requires_human); // Y6 forces human gate for entire proposal
        assert!(req.requires_shadow);
        assert_eq!(req.auto_approve_threshold, 0.10);
    }

    // ── Table structure ──

    #[test]
    fn test_default_gates_has_expected_count() {
        let g = gates();
        assert_eq!(g.len(), 12); // 12 rules in default config
        assert!(!g.is_empty());
    }

    #[test]
    fn test_schema_has_expected_columns() {
        let schema = safety_gates_schema();
        assert_eq!(schema.fields().len(), 7);
        assert_eq!(schema.field(0).name(), "gate_id");
        assert_eq!(schema.field(1).name(), "y_layer");
        assert_eq!(schema.field(6).name(), "description");
    }
}
