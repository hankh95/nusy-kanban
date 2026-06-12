//! Justification engine — evidence trails with source triples.
//!
//! EX-3241: Beings must explain their reasoning. The justification engine
//! produces an evidence trail for every response: which triples supported
//! the conclusion, which documents they came from, and what reasoning path
//! was followed. Safety-critical domains (medical, legal) REQUIRE
//! justification before a response is allowed.

use std::sync::Arc;

use arrow::array::{Float32Array, Int32Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;

// ── Column Constants ──────────────────────────────────────────────────────────

/// Named column indices for the justification schema.
/// Use these instead of hardcoded integers when accessing RecordBatch columns.
pub mod just_col {
    /// Unique trail ID (UUID string).
    pub const TRAIL_ID: usize = 0;
    /// Original query that prompted the conclusion.
    pub const QUERY: usize = 1;
    /// The conclusion reached.
    pub const CONCLUSION: usize = 2;
    /// Number of supporting triples.
    pub const TRIPLE_COUNT: usize = 3;
    /// Number of source chunks referenced.
    pub const CHUNK_COUNT: usize = 4;
    /// Confidence score [0.0, 1.0].
    pub const CONFIDENCE: usize = 5;
    /// Reasoning path steps joined with " → ".
    pub const REASONING_PATH: usize = 6;
    /// Domain label for the query.
    pub const DOMAIN: usize = 7;
    /// Verdict string (Sufficient/Insufficient/AddCaveat).
    pub const VERDICT: usize = 8;
    /// Timestamp when the trail was recorded (epoch millis).
    pub const TIMESTAMP: usize = 9;
}

// ── Error Type ────────────────────────────────────────────────────────────────

/// Errors from justification operations.
#[derive(Debug, Clone, PartialEq)]
pub enum JustificationError {
    /// Builder was missing required fields.
    MissingField(String),
    /// Arrow operation failed.
    ArrowError(String),
}

impl std::fmt::Display for JustificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JustificationError::MissingField(field) => {
                write!(f, "missing required field: {field}")
            }
            JustificationError::ArrowError(msg) => write!(f, "Arrow error: {msg}"),
        }
    }
}

impl std::error::Error for JustificationError {}

// ── Core Types ────────────────────────────────────────────────────────────────

/// Reference to a source chunk used as evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkRef {
    /// Unique identifier for the chunk.
    pub chunk_id: String,
    /// Document the chunk came from.
    pub document: String,
    /// Paragraph or section within the document.
    pub paragraph: String,
    /// Y-layer the chunk belongs to ("y0", "y1", "y2").
    pub y_layer: String,
}

/// An evidence trail supporting a conclusion.
#[derive(Debug, Clone)]
pub struct EvidenceTrail {
    /// The original query that prompted this trail.
    pub query: String,
    /// The conclusion reached from the evidence.
    pub conclusion: String,
    /// Supporting triples as (subject, predicate, object).
    pub supporting_triples: Vec<(String, String, String)>,
    /// Source chunks that contained the evidence.
    pub source_chunks: Vec<ChunkRef>,
    /// Confidence in the conclusion [0.0, 1.0].
    pub confidence: f32,
    /// Ordered reasoning steps that led to the conclusion.
    pub reasoning_path: Vec<String>,
}

/// Domain-specific justification policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JustificationPolicy {
    /// Medical, legal: trail MUST have >= 2 supporting triples.
    Required,
    /// Educational: trail recommended, caveat added if empty.
    Optional,
    /// Creative: no justification needed.
    Omitted,
}

impl JustificationPolicy {
    /// Classify a domain string into a justification policy.
    ///
    /// Aligns with `RiskTier::for_domain` in `battery.rs`:
    /// - SafetyCritical (medical, legal) → Required
    /// - Professional/Educational → Optional
    /// - Creative → Omitted
    pub fn for_domain(domain: &str) -> Self {
        let d = domain.to_lowercase();
        match d.as_str() {
            // Safety-critical domains: justification is mandatory
            "medical" | "usmle" | "diabetes" | "clinical" | "legal" | "bar_exam" | "bar-exam" => {
                JustificationPolicy::Required
            }
            // Creative domains: no justification needed
            "creative" | "3d_digital_artist" | "storytelling" | "games" => {
                JustificationPolicy::Omitted
            }
            // Everything else (professional, educational, unknown): optional
            _ => JustificationPolicy::Optional,
        }
    }
}

/// Result of justification gate check.
#[derive(Debug, Clone, PartialEq)]
pub enum JustificationVerdict {
    /// Trail meets domain requirements.
    Sufficient {
        /// Human-readable explanation of why the trail is sufficient.
        explanation: String,
    },
    /// Trail insufficient for this domain.
    Insufficient {
        /// Why the trail is insufficient.
        reason: String,
        /// The policy that was violated.
        policy: JustificationPolicy,
    },
    /// Caveat should be added (Optional policy with weak evidence).
    AddCaveat {
        /// The caveat text to prepend to the response.
        caveat: String,
    },
}

impl JustificationVerdict {
    /// Return a short label for Arrow serialization.
    fn label(&self) -> &str {
        match self {
            JustificationVerdict::Sufficient { .. } => "Sufficient",
            JustificationVerdict::Insufficient { .. } => "Insufficient",
            JustificationVerdict::AddCaveat { .. } => "AddCaveat",
        }
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Builder for constructing evidence trails incrementally.
///
/// Usage:
/// ```rust
/// use nusy_safety::justification::{JustificationBuilder, ChunkRef};
///
/// let trail = JustificationBuilder::for_query("What causes diabetes?")
///     .set_conclusion("Diabetes is caused by insulin dysfunction")
///     .add_triple(
///         "diabetes", "caused_by", "insulin_dysfunction",
///         ChunkRef {
///             chunk_id: "c1".into(),
///             document: "medical-textbook.md".into(),
///             paragraph: "Chapter 3, Section 1".into(),
///             y_layer: "y0".into(),
///         },
///     )
///     .add_reasoning_step("query received")
///     .add_reasoning_step("found 1 matching triple")
///     .add_reasoning_step("conclusion formed")
///     .build()
///     .unwrap();
/// ```
pub struct JustificationBuilder {
    query: String,
    conclusion: Option<String>,
    triples: Vec<(String, String, String)>,
    chunks: Vec<ChunkRef>,
    reasoning_steps: Vec<String>,
}

impl JustificationBuilder {
    /// Start building a trail for the given query.
    pub fn for_query(query: &str) -> Self {
        Self {
            query: query.to_string(),
            conclusion: None,
            triples: Vec::new(),
            chunks: Vec::new(),
            reasoning_steps: Vec::new(),
        }
    }

    /// Set the conclusion reached from the evidence.
    pub fn set_conclusion(mut self, conclusion: &str) -> Self {
        self.conclusion = Some(conclusion.to_string());
        self
    }

    /// Add a supporting triple and its source chunk.
    pub fn add_triple(
        mut self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: ChunkRef,
    ) -> Self {
        self.triples.push((
            subject.to_string(),
            predicate.to_string(),
            object.to_string(),
        ));
        self.chunks.push(source);
        self
    }

    /// Add a reasoning step to the trail.
    pub fn add_reasoning_step(mut self, step: &str) -> Self {
        self.reasoning_steps.push(step.to_string());
        self
    }

    /// Build the evidence trail, consuming the builder.
    ///
    /// Returns an error if required fields (query, conclusion) are missing.
    pub fn build(self) -> Result<EvidenceTrail, JustificationError> {
        if self.query.is_empty() {
            return Err(JustificationError::MissingField("query".to_string()));
        }

        let conclusion = self
            .conclusion
            .ok_or_else(|| JustificationError::MissingField("conclusion".to_string()))?;

        // Confidence based on evidence density: more triples = higher confidence
        let confidence = if self.triples.is_empty() {
            0.0
        } else {
            (self.triples.len() as f32 * 0.25).min(1.0)
        };

        Ok(EvidenceTrail {
            query: self.query,
            conclusion,
            supporting_triples: self.triples,
            source_chunks: self.chunks,
            confidence,
            reasoning_path: self.reasoning_steps,
        })
    }

    /// Produce a natural language explanation from the current builder state.
    ///
    /// This can be called before `build()` to preview the justification text.
    pub fn to_natural_language(&self) -> String {
        let conclusion = match &self.conclusion {
            Some(c) => c.as_str(),
            None => return "No conclusion set.".to_string(),
        };

        if self.triples.is_empty() {
            return "No supporting evidence found.".to_string();
        }

        let mut parts = Vec::new();
        for (i, ((subj, pred, obj), chunk)) in
            self.triples.iter().zip(self.chunks.iter()).enumerate()
        {
            parts.push(format!(
                "{}. ({} {} {}) from document \"{}\"",
                i + 1,
                subj,
                pred,
                obj,
                chunk.document
            ));
        }

        format!(
            "I concluded \"{}\" because: {}",
            conclusion,
            parts.join("; ")
        )
    }
}

// ── Gate ───────────────────────────────────────────────────────────────────────

/// Check whether an evidence trail satisfies the justification policy
/// for the given domain.
///
/// Rules:
/// - Required + >= 2 triples → Sufficient
/// - Required + < 2 triples → Insufficient
/// - Optional + 0 triples → AddCaveat("I'm not certain, but...")
/// - Optional + >= 1 triple → Sufficient
/// - Omitted → always Sufficient
pub fn check_justification(domain: &str, trail: &EvidenceTrail) -> JustificationVerdict {
    let policy = JustificationPolicy::for_domain(domain);
    let triple_count = trail.supporting_triples.len();

    match policy {
        JustificationPolicy::Required => {
            if triple_count >= 2 {
                JustificationVerdict::Sufficient {
                    explanation: format!(
                        "Trail has {} supporting triples (>= 2 required for {})",
                        triple_count, domain
                    ),
                }
            } else {
                JustificationVerdict::Insufficient {
                    reason: format!(
                        "Domain '{}' requires >= 2 supporting triples, found {}",
                        domain, triple_count
                    ),
                    policy,
                }
            }
        }
        JustificationPolicy::Optional => {
            if triple_count >= 1 {
                JustificationVerdict::Sufficient {
                    explanation: format!(
                        "Trail has {} supporting triple(s) for optional domain '{}'",
                        triple_count, domain
                    ),
                }
            } else {
                JustificationVerdict::AddCaveat {
                    caveat: "I'm not certain, but...".to_string(),
                }
            }
        }
        JustificationPolicy::Omitted => JustificationVerdict::Sufficient {
            explanation: format!("Justification omitted for '{}' domain", domain),
        },
    }
}

// ── Arrow Integration ─────────────────────────────────────────────────────────

/// Arrow schema for justification records.
///
/// Columns (10 total):
/// - `trail_id`: unique identifier (UUID string)
/// - `query`: original query text
/// - `conclusion`: the conclusion reached
/// - `triple_count`: number of supporting triples
/// - `chunk_count`: number of source chunks
/// - `confidence`: confidence score [0.0, 1.0]
/// - `reasoning_path`: steps joined with " → "
/// - `domain`: domain label
/// - `verdict`: Sufficient / Insufficient / AddCaveat
/// - `timestamp`: epoch milliseconds
pub fn justification_schema() -> Schema {
    Schema::new(vec![
        Field::new("trail_id", DataType::Utf8, false),
        Field::new("query", DataType::Utf8, false),
        Field::new("conclusion", DataType::Utf8, false),
        Field::new("triple_count", DataType::Int32, false),
        Field::new("chunk_count", DataType::Int32, false),
        Field::new("confidence", DataType::Float32, false),
        Field::new("reasoning_path", DataType::Utf8, false),
        Field::new("domain", DataType::Utf8, false),
        Field::new("verdict", DataType::Utf8, false),
        Field::new("timestamp", DataType::Int64, false),
    ])
}

/// Convert an evidence trail, domain, and verdict into an Arrow RecordBatch.
///
/// Each call produces a single-row batch conforming to `justification_schema()`.
pub fn trail_to_arrow(
    trail: &EvidenceTrail,
    domain: &str,
    verdict: &JustificationVerdict,
) -> Result<RecordBatch, JustificationError> {
    let trail_id = uuid::Uuid::new_v4().to_string();
    let reasoning_path_str = trail.reasoning_path.join(" → ");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let schema = Arc::new(justification_schema());

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![trail_id.as_str()])),
            Arc::new(StringArray::from(vec![trail.query.as_str()])),
            Arc::new(StringArray::from(vec![trail.conclusion.as_str()])),
            Arc::new(Int32Array::from(
                vec![trail.supporting_triples.len() as i32],
            )),
            Arc::new(Int32Array::from(vec![trail.source_chunks.len() as i32])),
            Arc::new(Float32Array::from(vec![trail.confidence])),
            Arc::new(StringArray::from(vec![reasoning_path_str.as_str()])),
            Arc::new(StringArray::from(vec![domain])),
            Arc::new(StringArray::from(vec![verdict.label()])),
            Arc::new(Int64Array::from(vec![now])),
        ],
    )
    .map_err(|e| JustificationError::ArrowError(e.to_string()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_chunk(id: &str, doc: &str) -> ChunkRef {
        ChunkRef {
            chunk_id: id.to_string(),
            document: doc.to_string(),
            paragraph: "p1".to_string(),
            y_layer: "y0".to_string(),
        }
    }

    // ── Builder tests ─────────────────────────────────────────────────────

    #[test]
    fn test_builder_basic() {
        let trail = JustificationBuilder::for_query("What causes diabetes?")
            .set_conclusion("Insulin dysfunction")
            .add_triple(
                "diabetes",
                "caused_by",
                "insulin_dysfunction",
                sample_chunk("c1", "textbook.md"),
            )
            .build();

        assert!(trail.is_ok());
        let trail = trail.expect("build should succeed");
        assert_eq!(trail.query, "What causes diabetes?");
        assert_eq!(trail.conclusion, "Insulin dysfunction");
        assert_eq!(trail.supporting_triples.len(), 1);
        assert_eq!(trail.source_chunks.len(), 1);
    }

    #[test]
    fn test_builder_no_conclusion_errors() {
        let result = JustificationBuilder::for_query("What causes diabetes?").build();

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            JustificationError::MissingField("conclusion".to_string())
        );
    }

    #[test]
    fn test_builder_empty_query_errors() {
        let result = JustificationBuilder::for_query("")
            .set_conclusion("something")
            .build();

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            JustificationError::MissingField("query".to_string())
        );
    }

    #[test]
    fn test_add_triple_with_chunk_ref() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .add_triple("s1", "p1", "o1", sample_chunk("c1", "doc1.md"))
            .add_triple("s2", "p2", "o2", sample_chunk("c2", "doc2.md"))
            .build()
            .expect("build should succeed");

        assert_eq!(trail.supporting_triples.len(), 2);
        assert_eq!(trail.source_chunks.len(), 2);
        assert_eq!(
            trail.supporting_triples[0],
            ("s1".to_string(), "p1".to_string(), "o1".to_string())
        );
        assert_eq!(trail.source_chunks[0].chunk_id, "c1");
        assert_eq!(trail.source_chunks[0].document, "doc1.md");
        assert_eq!(
            trail.supporting_triples[1],
            ("s2".to_string(), "p2".to_string(), "o2".to_string())
        );
        assert_eq!(trail.source_chunks[1].chunk_id, "c2");
    }

    #[test]
    fn test_add_reasoning_step() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .add_reasoning_step("query received")
            .add_reasoning_step("found 3 matching triples")
            .add_reasoning_step("conclusion formed")
            .build()
            .expect("build should succeed");

        assert_eq!(trail.reasoning_path.len(), 3);
        assert_eq!(trail.reasoning_path[0], "query received");
        assert_eq!(trail.reasoning_path[1], "found 3 matching triples");
        assert_eq!(trail.reasoning_path[2], "conclusion formed");
    }

    #[test]
    fn test_to_natural_language() {
        let builder = JustificationBuilder::for_query("What causes diabetes?")
            .set_conclusion("Insulin dysfunction")
            .add_triple(
                "diabetes",
                "caused_by",
                "insulin_dysfunction",
                sample_chunk("c1", "textbook.md"),
            );

        let nl = builder.to_natural_language();
        assert!(nl.contains("I concluded"));
        assert!(nl.contains("Insulin dysfunction"));
        assert!(nl.contains("diabetes"));
        assert!(nl.contains("caused_by"));
        assert!(nl.contains("insulin_dysfunction"));
        assert!(nl.contains("textbook.md"));
    }

    #[test]
    fn test_to_natural_language_empty() {
        let builder = JustificationBuilder::for_query("query").set_conclusion("conclusion");

        let nl = builder.to_natural_language();
        assert_eq!(nl, "No supporting evidence found.");
    }

    // ── Policy tests ──────────────────────────────────────────────────────

    #[test]
    fn test_policy_medical_required() {
        assert_eq!(
            JustificationPolicy::for_domain("medical"),
            JustificationPolicy::Required
        );
    }

    #[test]
    fn test_policy_legal_required() {
        assert_eq!(
            JustificationPolicy::for_domain("legal"),
            JustificationPolicy::Required
        );
    }

    #[test]
    fn test_policy_educational_optional() {
        assert_eq!(
            JustificationPolicy::for_domain("highschool"),
            JustificationPolicy::Optional
        );
    }

    #[test]
    fn test_policy_creative_omitted() {
        assert_eq!(
            JustificationPolicy::for_domain("creative"),
            JustificationPolicy::Omitted
        );
    }

    #[test]
    fn test_policy_unknown_optional() {
        assert_eq!(
            JustificationPolicy::for_domain("unknown_domain"),
            JustificationPolicy::Optional
        );
    }

    // ── Gate tests ────────────────────────────────────────────────────────

    #[test]
    fn test_gate_required_sufficient() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .add_triple("s1", "p1", "o1", sample_chunk("c1", "d1.md"))
            .add_triple("s2", "p2", "o2", sample_chunk("c2", "d2.md"))
            .build()
            .expect("build");

        let verdict = check_justification("medical", &trail);
        match verdict {
            JustificationVerdict::Sufficient { explanation } => {
                assert!(explanation.contains("2"));
            }
            other => panic!("Expected Sufficient, got {other:?}"),
        }
    }

    #[test]
    fn test_gate_required_insufficient() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .add_triple("s1", "p1", "o1", sample_chunk("c1", "d1.md"))
            .build()
            .expect("build");

        let verdict = check_justification("medical", &trail);
        match verdict {
            JustificationVerdict::Insufficient { reason, policy } => {
                assert!(reason.contains("1"));
                assert_eq!(policy, JustificationPolicy::Required);
            }
            other => panic!("Expected Insufficient, got {other:?}"),
        }
    }

    #[test]
    fn test_gate_optional_caveat() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .build()
            .expect("build");

        let verdict = check_justification("highschool", &trail);
        match verdict {
            JustificationVerdict::AddCaveat { caveat } => {
                assert!(caveat.contains("not certain"));
            }
            other => panic!("Expected AddCaveat, got {other:?}"),
        }
    }

    #[test]
    fn test_gate_optional_sufficient() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .add_triple("s1", "p1", "o1", sample_chunk("c1", "d1.md"))
            .build()
            .expect("build");

        let verdict = check_justification("highschool", &trail);
        match verdict {
            JustificationVerdict::Sufficient { explanation } => {
                assert!(explanation.contains("1"));
            }
            other => panic!("Expected Sufficient, got {other:?}"),
        }
    }

    #[test]
    fn test_gate_omitted_always_sufficient() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .build()
            .expect("build");

        let verdict = check_justification("creative", &trail);
        match verdict {
            JustificationVerdict::Sufficient { explanation } => {
                assert!(explanation.contains("creative"));
            }
            other => panic!("Expected Sufficient, got {other:?}"),
        }
    }

    // ── Arrow tests ───────────────────────────────────────────────────────

    #[test]
    fn test_trail_to_arrow_schema() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .add_triple("s1", "p1", "o1", sample_chunk("c1", "d1.md"))
            .build()
            .expect("build");

        let verdict = check_justification("medical", &trail);
        let batch = trail_to_arrow(&trail, "medical", &verdict).expect("arrow conversion");

        let schema = batch.schema();
        assert_eq!(schema.fields().len(), 10);

        // Verify column names and types
        assert_eq!(schema.field(just_col::TRAIL_ID).name(), "trail_id");
        assert_eq!(
            schema.field(just_col::TRAIL_ID).data_type(),
            &DataType::Utf8
        );

        assert_eq!(schema.field(just_col::QUERY).name(), "query");
        assert_eq!(schema.field(just_col::QUERY).data_type(), &DataType::Utf8);

        assert_eq!(schema.field(just_col::CONCLUSION).name(), "conclusion");
        assert_eq!(
            schema.field(just_col::CONCLUSION).data_type(),
            &DataType::Utf8
        );

        assert_eq!(schema.field(just_col::TRIPLE_COUNT).name(), "triple_count");
        assert_eq!(
            schema.field(just_col::TRIPLE_COUNT).data_type(),
            &DataType::Int32
        );

        assert_eq!(schema.field(just_col::CHUNK_COUNT).name(), "chunk_count");
        assert_eq!(
            schema.field(just_col::CHUNK_COUNT).data_type(),
            &DataType::Int32
        );

        assert_eq!(schema.field(just_col::CONFIDENCE).name(), "confidence");
        assert_eq!(
            schema.field(just_col::CONFIDENCE).data_type(),
            &DataType::Float32
        );

        assert_eq!(
            schema.field(just_col::REASONING_PATH).name(),
            "reasoning_path"
        );
        assert_eq!(
            schema.field(just_col::REASONING_PATH).data_type(),
            &DataType::Utf8
        );

        assert_eq!(schema.field(just_col::DOMAIN).name(), "domain");
        assert_eq!(schema.field(just_col::DOMAIN).data_type(), &DataType::Utf8);

        assert_eq!(schema.field(just_col::VERDICT).name(), "verdict");
        assert_eq!(schema.field(just_col::VERDICT).data_type(), &DataType::Utf8);

        assert_eq!(schema.field(just_col::TIMESTAMP).name(), "timestamp");
        assert_eq!(
            schema.field(just_col::TIMESTAMP).data_type(),
            &DataType::Int64
        );
    }

    #[test]
    fn test_trail_to_arrow_values() {
        let trail = JustificationBuilder::for_query("What causes diabetes?")
            .set_conclusion("Insulin dysfunction")
            .add_triple(
                "diabetes",
                "caused_by",
                "insulin_dysfunction",
                sample_chunk("c1", "textbook.md"),
            )
            .add_triple(
                "insulin",
                "regulates",
                "blood_sugar",
                sample_chunk("c2", "reference.md"),
            )
            .add_reasoning_step("query received")
            .add_reasoning_step("found 2 matching triples")
            .add_reasoning_step("conclusion formed")
            .build()
            .expect("build");

        let verdict = JustificationVerdict::Sufficient {
            explanation: "test".to_string(),
        };
        let batch = trail_to_arrow(&trail, "medical", &verdict).expect("arrow conversion");

        assert_eq!(batch.num_rows(), 1);

        // Verify values
        let conclusion_col = batch
            .column(just_col::CONCLUSION)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("conclusion is StringArray");
        assert_eq!(conclusion_col.value(0), "Insulin dysfunction");

        let triple_count_col = batch
            .column(just_col::TRIPLE_COUNT)
            .as_any()
            .downcast_ref::<Int32Array>()
            .expect("triple_count is Int32Array");
        assert_eq!(triple_count_col.value(0), 2);

        let chunk_count_col = batch
            .column(just_col::CHUNK_COUNT)
            .as_any()
            .downcast_ref::<Int32Array>()
            .expect("chunk_count is Int32Array");
        assert_eq!(chunk_count_col.value(0), 2);

        let confidence_col = batch
            .column(just_col::CONFIDENCE)
            .as_any()
            .downcast_ref::<Float32Array>()
            .expect("confidence is Float32Array");
        assert!((confidence_col.value(0) - 0.5).abs() < f32::EPSILON);

        let reasoning_col = batch
            .column(just_col::REASONING_PATH)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("reasoning_path is StringArray");
        assert_eq!(
            reasoning_col.value(0),
            "query received → found 2 matching triples → conclusion formed"
        );

        let domain_col = batch
            .column(just_col::DOMAIN)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("domain is StringArray");
        assert_eq!(domain_col.value(0), "medical");

        let verdict_col = batch
            .column(just_col::VERDICT)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("verdict is StringArray");
        assert_eq!(verdict_col.value(0), "Sufficient");

        let timestamp_col = batch
            .column(just_col::TIMESTAMP)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("timestamp is Int64Array");
        assert!(timestamp_col.value(0) > 0, "timestamp should be positive");

        // Verify the query field round-trips through Arrow (EX-3255)
        let query_col = batch
            .column(just_col::QUERY)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("query is StringArray");
        assert_eq!(
            query_col.value(0),
            "What causes diabetes?",
            "query must be written from EvidenceTrail.query, not empty string"
        );
    }

    #[test]
    fn test_query_field_round_trips_through_arrow() {
        // Regression: trail_to_arrow() previously wrote "" for query (EX-3255 bug).
        let query = "What is the mechanism of insulin resistance?";
        let trail = JustificationBuilder::for_query(query)
            .set_conclusion("Insulin receptor downregulation")
            .add_triple("insulin", "binds", "receptor", sample_chunk("c1", "d1.md"))
            .build()
            .expect("build");

        assert_eq!(
            trail.query, query,
            "EvidenceTrail.query must match builder query"
        );

        let verdict = JustificationVerdict::Sufficient {
            explanation: "ok".to_string(),
        };
        let batch = trail_to_arrow(&trail, "medical", &verdict).expect("arrow");
        let query_col = batch
            .column(just_col::QUERY)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("query col");
        assert_eq!(
            query_col.value(0),
            query,
            "Arrow query column must contain the original query string"
        );
    }

    #[test]
    fn test_to_natural_language_multi_triple() {
        // EX-3255: to_natural_language previously only tested with a single triple.
        let builder = JustificationBuilder::for_query("Why is blood pressure elevated?")
            .set_conclusion("Hypertension due to multiple factors")
            .add_triple(
                "stress",
                "raises",
                "cortisol",
                sample_chunk("c1", "medical.md"),
            )
            .add_triple(
                "cortisol",
                "increases",
                "blood_pressure",
                sample_chunk("c2", "physiology.md"),
            )
            .add_triple(
                "sodium",
                "retains",
                "fluid",
                sample_chunk("c3", "nutrition.md"),
            );

        let nl = builder.to_natural_language();
        assert!(
            nl.contains("I concluded"),
            "should start with conclusion prefix"
        );
        assert!(
            nl.contains("Hypertension"),
            "should include conclusion text"
        );
        // All three evidence items should appear
        assert!(nl.contains("1."), "triple 1 should be numbered");
        assert!(nl.contains("2."), "triple 2 should be numbered");
        assert!(nl.contains("3."), "triple 3 should be numbered");
        assert!(nl.contains("medical.md"), "doc from triple 1");
        assert!(nl.contains("physiology.md"), "doc from triple 2");
        assert!(nl.contains("nutrition.md"), "doc from triple 3");
    }

    #[test]
    fn test_confidence_in_trail() {
        // No triples → 0.0 confidence
        let trail_empty = JustificationBuilder::for_query("q")
            .set_conclusion("c")
            .build()
            .expect("build");
        assert!((trail_empty.confidence - 0.0).abs() < f32::EPSILON);

        // 1 triple → 0.25 confidence
        let trail_one = JustificationBuilder::for_query("q")
            .set_conclusion("c")
            .add_triple("s", "p", "o", sample_chunk("c1", "d.md"))
            .build()
            .expect("build");
        assert!((trail_one.confidence - 0.25).abs() < f32::EPSILON);

        // 4 triples → 1.0 confidence (capped)
        let trail_four = JustificationBuilder::for_query("q")
            .set_conclusion("c")
            .add_triple("s1", "p1", "o1", sample_chunk("c1", "d.md"))
            .add_triple("s2", "p2", "o2", sample_chunk("c2", "d.md"))
            .add_triple("s3", "p3", "o3", sample_chunk("c3", "d.md"))
            .add_triple("s4", "p4", "o4", sample_chunk("c4", "d.md"))
            .build()
            .expect("build");
        assert!((trail_four.confidence - 1.0).abs() < f32::EPSILON);

        // 5 triples → still 1.0 (capped at max)
        let trail_five = JustificationBuilder::for_query("q")
            .set_conclusion("c")
            .add_triple("s1", "p1", "o1", sample_chunk("c1", "d.md"))
            .add_triple("s2", "p2", "o2", sample_chunk("c2", "d.md"))
            .add_triple("s3", "p3", "o3", sample_chunk("c3", "d.md"))
            .add_triple("s4", "p4", "o4", sample_chunk("c4", "d.md"))
            .add_triple("s5", "p5", "o5", sample_chunk("c5", "d.md"))
            .build()
            .expect("build");
        assert!((trail_five.confidence - 1.0).abs() < f32::EPSILON);
    }

    // ── Edge case tests ───────────────────────────────────────────────────

    #[test]
    fn test_natural_language_no_conclusion() {
        let builder = JustificationBuilder::for_query("query");
        assert_eq!(builder.to_natural_language(), "No conclusion set.");
    }

    #[test]
    fn test_gate_required_zero_triples_insufficient() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .build()
            .expect("build");

        let verdict = check_justification("legal", &trail);
        match verdict {
            JustificationVerdict::Insufficient { reason, policy } => {
                assert!(reason.contains("0"));
                assert_eq!(policy, JustificationPolicy::Required);
            }
            other => panic!("Expected Insufficient, got {other:?}"),
        }
    }

    #[test]
    fn test_gate_required_three_triples_sufficient() {
        let trail = JustificationBuilder::for_query("query")
            .set_conclusion("conclusion")
            .add_triple("s1", "p1", "o1", sample_chunk("c1", "d1.md"))
            .add_triple("s2", "p2", "o2", sample_chunk("c2", "d2.md"))
            .add_triple("s3", "p3", "o3", sample_chunk("c3", "d3.md"))
            .build()
            .expect("build");

        let verdict = check_justification("medical", &trail);
        assert!(matches!(verdict, JustificationVerdict::Sufficient { .. }));
    }

    #[test]
    fn test_policy_bar_exam_required() {
        assert_eq!(
            JustificationPolicy::for_domain("bar_exam"),
            JustificationPolicy::Required
        );
        assert_eq!(
            JustificationPolicy::for_domain("bar-exam"),
            JustificationPolicy::Required
        );
    }

    #[test]
    fn test_policy_case_insensitive() {
        assert_eq!(
            JustificationPolicy::for_domain("Medical"),
            JustificationPolicy::Required
        );
        assert_eq!(
            JustificationPolicy::for_domain("CREATIVE"),
            JustificationPolicy::Omitted
        );
    }

    #[test]
    fn test_chunk_ref_equality() {
        let a = ChunkRef {
            chunk_id: "c1".into(),
            document: "doc.md".into(),
            paragraph: "p1".into(),
            y_layer: "y0".into(),
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn test_verdict_label() {
        assert_eq!(
            JustificationVerdict::Sufficient {
                explanation: "ok".into()
            }
            .label(),
            "Sufficient"
        );
        assert_eq!(
            JustificationVerdict::Insufficient {
                reason: "bad".into(),
                policy: JustificationPolicy::Required
            }
            .label(),
            "Insufficient"
        );
        assert_eq!(
            JustificationVerdict::AddCaveat {
                caveat: "maybe".into()
            }
            .label(),
            "AddCaveat"
        );
    }
}
