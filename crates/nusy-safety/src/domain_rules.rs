//! Domain Justification Rules — configurable per-domain evidence requirements (V23-6, EX-3342).
//!
//! Each domain has configurable thresholds for what constitutes sufficient justification:
//! minimum triple count, minimum chunk count, minimum confidence, and whether human review
//! is required. Built-in rules cover medical, legal, ethical, and educational domains.
//!
//! # Example
//! ```
//! use nusy_safety::domain_rules::{DomainRuleStore, DomainVerdict};
//! use nusy_safety::justification::{JustificationBuilder, ChunkRef};
//!
//! let store = DomainRuleStore::with_defaults();
//! let trail = JustificationBuilder::for_query("What is the treatment for X?")
//!     .set_conclusion("Treatment is Y")
//!     .add_triple("X", "treated_by", "Y",
//!         ChunkRef { chunk_id: "c1".into(), document: "textbook.md".into(),
//!                    paragraph: "Ch3".into(), y_layer: "y0".into() })
//!     .add_triple("Y", "evidence_level", "strong",
//!         ChunkRef { chunk_id: "c2".into(), document: "meta-analysis.md".into(),
//!                    paragraph: "Results".into(), y_layer: "y1".into() })
//!     .add_triple("Y", "mechanism", "enzyme_inhibition",
//!         ChunkRef { chunk_id: "c3".into(), document: "pharmacology.md".into(),
//!                    paragraph: "MOA".into(), y_layer: "y1".into() })
//!     .build().unwrap();
//! let verdict = store.evaluate(&trail, "medical");
//! assert!(matches!(verdict, DomainVerdict::Approved));
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use arrow::array::{
    BooleanArray, BooleanBuilder, Float32Array, Float32Builder, Int32Array, Int32Builder,
    RecordBatch, StringArray, StringBuilder,
};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};

use crate::justification::EvidenceTrail;

// ── Schema ──────────────────────────────────────────────────────────────────

/// Arrow schema for domain justification rules.
pub fn domain_rules_schema() -> Schema {
    Schema::new(vec![
        Field::new("domain", DataType::Utf8, false),
        Field::new("min_triples", DataType::Int32, false),
        Field::new("min_chunks", DataType::Int32, false),
        Field::new("min_confidence", DataType::Float32, false),
        Field::new("requires_human_review", DataType::Boolean, false),
    ])
}

fn schema_ref() -> SchemaRef {
    Arc::new(domain_rules_schema())
}

// ── Rule Record ─────────────────────────────────────────────────────────────

/// Configuration for a single domain's justification requirements.
#[derive(Debug, Clone)]
pub struct DomainRule {
    /// Domain identifier (e.g., "medical", "legal").
    pub domain: String,
    /// Minimum number of supporting triples required.
    pub min_triples: i32,
    /// Minimum number of source chunks required.
    pub min_chunks: i32,
    /// Minimum confidence score [0.0, 1.0].
    pub min_confidence: f32,
    /// Whether a human must review before the response is transmitted.
    pub requires_human_review: bool,
}

// ── Verdict ─────────────────────────────────────────────────────────────────

/// Result of evaluating an evidence trail against domain rules.
#[derive(Debug, Clone, PartialEq)]
pub enum DomainVerdict {
    /// Trail meets all domain requirements.
    Approved,
    /// Trail does not meet domain requirements.
    InsufficientEvidence {
        /// Human-readable explanation of what's missing.
        reason: String,
    },
    /// Trail meets evidence requirements but needs human review.
    RequiresHumanReview,
}

// ── Store ───────────────────────────────────────────────────────────────────

/// Arrow-backed store for domain-specific justification rules.
pub struct DomainRuleStore {
    rules: HashMap<String, DomainRule>,
}

impl DomainRuleStore {
    /// Create an empty rule store.
    pub fn new() -> Self {
        Self {
            rules: HashMap::new(),
        }
    }

    /// Create a store pre-loaded with the 4 built-in domain rules.
    pub fn with_defaults() -> Self {
        let mut store = Self::new();

        store.add_rule(DomainRule {
            domain: "medical".into(),
            min_triples: 3,
            min_chunks: 2,
            min_confidence: 0.8,
            requires_human_review: true,
        });
        store.add_rule(DomainRule {
            domain: "legal".into(),
            min_triples: 2,
            min_chunks: 1,
            min_confidence: 0.7,
            requires_human_review: true,
        });
        store.add_rule(DomainRule {
            domain: "ethical".into(),
            min_triples: 2,
            min_chunks: 1,
            min_confidence: 0.6,
            requires_human_review: false,
        });
        store.add_rule(DomainRule {
            domain: "educational".into(),
            min_triples: 1,
            min_chunks: 1,
            min_confidence: 0.5,
            requires_human_review: false,
        });

        store
    }

    /// Add or replace a domain rule.
    pub fn add_rule(&mut self, rule: DomainRule) {
        self.rules.insert(rule.domain.clone(), rule);
    }

    /// Look up a rule by domain.
    pub fn get(&self, domain: &str) -> Option<&DomainRule> {
        self.rules.get(domain)
    }

    /// Number of configured rules.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether no rules are configured.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Evaluate an evidence trail against the domain's rules.
    ///
    /// If the domain has no configured rule, returns `Approved` (permissive fallback).
    pub fn evaluate(&self, trail: &EvidenceTrail, domain: &str) -> DomainVerdict {
        let rule = match self.rules.get(domain) {
            Some(r) => r,
            None => return DomainVerdict::Approved, // Unknown domain → permissive
        };

        let triple_count = trail.supporting_triples.len() as i32;
        let chunk_count = trail.source_chunks.len() as i32;

        // Check evidence thresholds
        let mut deficiencies = Vec::new();

        if triple_count < rule.min_triples {
            deficiencies.push(format!(
                "need {} triples, have {}",
                rule.min_triples, triple_count
            ));
        }

        if chunk_count < rule.min_chunks {
            deficiencies.push(format!(
                "need {} source chunks, have {}",
                rule.min_chunks, chunk_count
            ));
        }

        if trail.confidence < rule.min_confidence {
            deficiencies.push(format!(
                "need confidence >= {:.1}, have {:.2}",
                rule.min_confidence, trail.confidence
            ));
        }

        if !deficiencies.is_empty() {
            return DomainVerdict::InsufficientEvidence {
                reason: format!(
                    "Domain '{}' requirements not met: {}",
                    domain,
                    deficiencies.join("; ")
                ),
            };
        }

        // Evidence is sufficient — check if human review required
        if rule.requires_human_review {
            return DomainVerdict::RequiresHumanReview;
        }

        DomainVerdict::Approved
    }

    /// Evaluate and also check that source chunks use allowed Y-layers.
    ///
    /// If `allowed_layers` is empty, all layers are accepted.
    pub fn evaluate_with_layers(
        &self,
        trail: &EvidenceTrail,
        domain: &str,
        allowed_layers: &HashSet<String>,
    ) -> DomainVerdict {
        // First check basic evidence thresholds
        let base_verdict = self.evaluate(trail, domain);
        if matches!(base_verdict, DomainVerdict::InsufficientEvidence { .. }) {
            return base_verdict;
        }

        // Check Y-layer restrictions if any
        if !allowed_layers.is_empty() {
            for chunk in &trail.source_chunks {
                if !allowed_layers.contains(&chunk.y_layer) {
                    return DomainVerdict::InsufficientEvidence {
                        reason: format!(
                            "Domain '{}' restricts Y-layers to {:?}, but chunk '{}' uses '{}'",
                            domain, allowed_layers, chunk.chunk_id, chunk.y_layer
                        ),
                    };
                }
            }
        }

        base_verdict
    }

    // ── Arrow Serialization ────────────────────────────────────────────────

    /// Serialize all rules to an Arrow RecordBatch.
    pub fn to_batch(&self) -> Result<RecordBatch, String> {
        let mut domains = StringBuilder::new();
        let mut min_triples = Int32Builder::new();
        let mut min_chunks = Int32Builder::new();
        let mut min_confidences = Float32Builder::new();
        let mut human_reviews = BooleanBuilder::new();

        let mut sorted: Vec<&DomainRule> = self.rules.values().collect();
        sorted.sort_by(|a, b| a.domain.cmp(&b.domain));

        for rule in sorted {
            domains.append_value(&rule.domain);
            min_triples.append_value(rule.min_triples);
            min_chunks.append_value(rule.min_chunks);
            min_confidences.append_value(rule.min_confidence);
            human_reviews.append_value(rule.requires_human_review);
        }

        RecordBatch::try_new(
            schema_ref(),
            vec![
                Arc::new(domains.finish()),
                Arc::new(min_triples.finish()),
                Arc::new(min_chunks.finish()),
                Arc::new(min_confidences.finish()),
                Arc::new(human_reviews.finish()),
            ],
        )
        .map_err(|e| e.to_string())
    }

    /// Deserialize rules from an Arrow RecordBatch.
    pub fn from_batch(batch: &RecordBatch) -> Result<Self, String> {
        let domains = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("domain column type mismatch")?;
        let min_triples = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int32Array>()
            .ok_or("min_triples column type mismatch")?;
        let min_chunks = batch
            .column(2)
            .as_any()
            .downcast_ref::<Int32Array>()
            .ok_or("min_chunks column type mismatch")?;
        let min_confidences = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float32Array>()
            .ok_or("min_confidence column type mismatch")?;
        let human_reviews = batch
            .column(4)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .ok_or("requires_human_review column type mismatch")?;

        let mut store = Self::new();
        for i in 0..batch.num_rows() {
            store.add_rule(DomainRule {
                domain: domains.value(i).to_string(),
                min_triples: min_triples.value(i),
                min_chunks: min_chunks.value(i),
                min_confidence: min_confidences.value(i),
                requires_human_review: human_reviews.value(i),
            });
        }
        Ok(store)
    }
}

impl Default for DomainRuleStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::justification::{ChunkRef, JustificationBuilder};

    fn chunk(id: &str, layer: &str) -> ChunkRef {
        ChunkRef {
            chunk_id: id.into(),
            document: "doc.md".into(),
            paragraph: "p1".into(),
            y_layer: layer.into(),
        }
    }

    fn medical_trail(triples: usize, confidence: f32) -> EvidenceTrail {
        let mut builder = JustificationBuilder::for_query("What is the treatment?")
            .set_conclusion("Treatment is X");

        for i in 0..triples {
            builder = builder.add_triple(
                &format!("s{i}"),
                "treated_by",
                &format!("o{i}"),
                chunk(&format!("c{i}"), "y0"),
            );
        }

        let mut trail = builder.build().unwrap();
        trail.confidence = confidence; // Override auto-calculated confidence
        trail
    }

    #[test]
    fn test_medical_approved_with_human_review() {
        let store = DomainRuleStore::with_defaults();
        let trail = medical_trail(3, 0.9);
        let verdict = store.evaluate(&trail, "medical");
        assert_eq!(verdict, DomainVerdict::RequiresHumanReview);
    }

    #[test]
    fn test_medical_insufficient_triples() {
        let store = DomainRuleStore::with_defaults();
        let trail = medical_trail(1, 0.9);
        let verdict = store.evaluate(&trail, "medical");
        assert!(matches!(
            verdict,
            DomainVerdict::InsufficientEvidence { .. }
        ));
        if let DomainVerdict::InsufficientEvidence { reason } = verdict {
            assert!(
                reason.contains("triples"),
                "Should mention triples: {reason}"
            );
        }
    }

    #[test]
    fn test_medical_insufficient_confidence() {
        let store = DomainRuleStore::with_defaults();
        let trail = medical_trail(3, 0.5);
        let verdict = store.evaluate(&trail, "medical");
        assert!(matches!(
            verdict,
            DomainVerdict::InsufficientEvidence { .. }
        ));
        if let DomainVerdict::InsufficientEvidence { reason } = verdict {
            assert!(
                reason.contains("confidence"),
                "Should mention confidence: {reason}"
            );
        }
    }

    #[test]
    fn test_legal_sufficient_with_human_review() {
        let store = DomainRuleStore::with_defaults();
        let trail = medical_trail(2, 0.8);
        let verdict = store.evaluate(&trail, "legal");
        assert_eq!(verdict, DomainVerdict::RequiresHumanReview);
    }

    #[test]
    fn test_educational_approved_no_review() {
        let store = DomainRuleStore::with_defaults();
        let trail = medical_trail(1, 0.6);
        let verdict = store.evaluate(&trail, "educational");
        assert_eq!(verdict, DomainVerdict::Approved);
    }

    #[test]
    fn test_unknown_domain_permissive() {
        let store = DomainRuleStore::with_defaults();
        let trail = medical_trail(0, 0.0);
        let verdict = store.evaluate(&trail, "unknown_domain");
        assert_eq!(verdict, DomainVerdict::Approved);
    }

    #[test]
    fn test_ylayer_restriction() {
        let store = DomainRuleStore::with_defaults();
        let trail = medical_trail(3, 0.9);
        let allowed: HashSet<String> = ["y0".into(), "y1".into()].into();
        let verdict = store.evaluate_with_layers(&trail, "medical", &allowed);
        // All chunks are y0 which is allowed → human review (evidence passes)
        assert_eq!(verdict, DomainVerdict::RequiresHumanReview);
    }

    #[test]
    fn test_ylayer_restriction_fails() {
        let store = DomainRuleStore::with_defaults();

        // Build a trail where one chunk uses y4 (not allowed)
        let trail_builder = JustificationBuilder::for_query("Test")
            .set_conclusion("Conclusion")
            .add_triple("s", "p", "o", chunk("c1", "y0"))
            .add_triple("s2", "p2", "o2", chunk("c2", "y4")) // disallowed
            .add_triple("s3", "p3", "o3", chunk("c3", "y0"));
        let mut trail = trail_builder.build().unwrap();
        trail.confidence = 0.9;

        let allowed: HashSet<String> = ["y0".into(), "y1".into()].into();
        let verdict = store.evaluate_with_layers(&trail, "medical", &allowed);
        assert!(matches!(
            verdict,
            DomainVerdict::InsufficientEvidence { .. }
        ));
        if let DomainVerdict::InsufficientEvidence { reason } = verdict {
            assert!(
                reason.contains("y4"),
                "Should mention disallowed layer: {reason}"
            );
        }
    }

    #[test]
    fn test_round_trip_arrow() {
        let store = DomainRuleStore::with_defaults();
        let batch = store.to_batch().expect("serialize");
        assert_eq!(batch.num_rows(), 4);

        let restored = DomainRuleStore::from_batch(&batch).expect("deserialize");
        assert_eq!(restored.len(), 4);

        let medical = restored.get("medical").expect("medical rule");
        assert_eq!(medical.min_triples, 3);
        assert_eq!(medical.min_chunks, 2);
        assert!((medical.min_confidence - 0.8).abs() < 0.001);
        assert!(medical.requires_human_review);

        let educational = restored.get("educational").expect("educational rule");
        assert_eq!(educational.min_triples, 1);
        assert!(!educational.requires_human_review);
    }

    #[test]
    fn test_custom_rule() {
        let mut store = DomainRuleStore::new();
        store.add_rule(DomainRule {
            domain: "financial".into(),
            min_triples: 5,
            min_chunks: 3,
            min_confidence: 0.95,
            requires_human_review: true,
        });

        let trail = medical_trail(4, 0.9);
        let verdict = store.evaluate(&trail, "financial");
        assert!(matches!(
            verdict,
            DomainVerdict::InsufficientEvidence { .. }
        ));
    }

    #[test]
    fn test_defaults_have_4_domains() {
        let store = DomainRuleStore::with_defaults();
        assert_eq!(store.len(), 4);
        assert!(store.get("medical").is_some());
        assert!(store.get("legal").is_some());
        assert!(store.get("ethical").is_some());
        assert!(store.get("educational").is_some());
    }

    #[test]
    fn test_schema_columns() {
        let schema = domain_rules_schema();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            names,
            vec![
                "domain",
                "min_triples",
                "min_chunks",
                "min_confidence",
                "requires_human_review"
            ]
        );
    }
}
