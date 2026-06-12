//! # nusy-tutor-record
//!
//! Canonical schema and validator for V16 Customer's Plate (5-layer tutor record).
//!
//! Lineage: V6/V7 Sushi Pipeline Step 2 (Customer's Plate) + Step 7 (Taste the
//! Sushi). One [`TutorRecord`] describes the expected output for a being reading
//! one document. The record is consumed downstream by:
//!
//! - **EX-iii** review tooling (CLI to view/edit/approve plates)
//! - **EX-iv** scenarios-pass evaluator (Step 7 — Taste the Sushi)
//! - **EX-α** cortex API (the cortex's 5-layer output is graded against the plate)
//!
//! The canonical artifact is the JSON Schema at
//! `schemas/tutor-record-v1.json`. Rust types here are one implementation of
//! that schema; serde guarantees structural validation, and
//! [`validate_against_schema`] runs the JSON Schema validator at runtime for
//! cross-language consumers.
//!
//! ## Quick start
//!
//! ```no_run
//! use nusy_tutor_record::TutorRecord;
//!
//! let json = std::fs::read_to_string("examples/dame_wonder.json").unwrap();
//! let record: TutorRecord = serde_json::from_str(&json).unwrap();
//! record.validate_semantics().unwrap();
//! println!("{} layer-1 triples", record.layers.layer_1_literal.triples.len());
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod review;
pub use review::{REVIEW_SCHEMA_VERSION, ReviewSignoff, ReviewStatus, sha256_hex};

/// Current schema version. Bump on incompatible structural changes.
pub const SCHEMA_VERSION: &str = "1.0";

/// Embedded JSON Schema text (the canonical contract). Used by
/// [`validate_against_schema`] so callers don't have to ship the schema file.
pub const SCHEMA_JSON: &str = include_str!("../schemas/tutor-record-v1.json");

/// One Customer's Plate. The full expected 5-layer output for one document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TutorRecord {
    pub schema_version: String,
    pub document: DocumentRef,
    pub tutor: TutorIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gist: Option<String>,
    pub layers: Layers,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoring: Option<Scoring>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// blake3 hash of the pinned source the plate was generated from (EX-4419 /
    /// CH-4428 G2 provenance). Optional — older plates predate source hashing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    /// Path to the pinned source the plate was generated from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// Free-text note on source provenance (e.g. format/path conversion details).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DocumentRef {
    pub path: String,
    pub level: Level,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub era: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_attribution: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum Level {
    L0_toddler,
    L1_grade_school,
    L2_middle_school,
    L3_high_school,
    L4_undergraduate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TutorIdentity {
    pub name: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_lineage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captain_reframe: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layers {
    pub layer_1_literal: Layer1Literal,
    pub layer_2_ontology: Layer2Ontology,
    pub layer_3_curiosity: Layer3Curiosity,
    /// Y3 practice exercises (EX-4463): scan_structure instances extracted from
    /// math/procedural content — individual exercise items, NOT Y1/Y2 facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_3_practice: Option<serde_json::Value>,
    pub layer_4_cross_book: Layer4CrossBook,
    pub layer_5_multimodal: Layer5Multimodal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer1Literal {
    pub chunks: Vec<Y0Chunk>,
    pub triples: Vec<Triple>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Y0Chunk {
    pub id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_lines: Option<LineRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salience: Option<Salience>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Salience {
    High,
    Medium,
    Low,
    Skip,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer2Ontology {
    pub entities: Vec<OntologyEntity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OntologyEntity {
    pub entity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wikidata_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conceptnet_uri: Option<String>,
    pub triples: Vec<Triple>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer3Curiosity {
    pub cqs: Vec<OpenCq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OpenCq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub cq: String,
    pub kind: CqKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_resolution: Option<Vec<Triple>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_resolution_chain: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_set: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CqKind {
    DirectWordMeaning,
    CausalChain,
    PatternRecognition,
    CrossStanzaRelational,
    Multimodal,
    Metacognitive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer4CrossBook {
    pub anchors: Vec<CrossBookAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CrossBookAnchor {
    pub anchor: String,
    pub fires_when_reading: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_link_to: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer5Multimodal {
    pub illustrations: Vec<Illustration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cadence: Option<CadenceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Illustration {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub expected_depicts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_triples: Option<Vec<Triple>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_modal_anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CadenceMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reading_meter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alliteration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rhyme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Scoring {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recall_target: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision_target: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cq_hit_target: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_close_target: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

// ─── Errors ─────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("schema_version mismatch: expected {expected}, got {got}")]
    SchemaVersionMismatch { expected: String, got: String },

    #[error("LineRange invalid: start ({start}) > end ({end})")]
    LineRangeInverted { start: usize, end: usize },

    #[error("duplicate chunk id: {0}")]
    DuplicateChunkId(String),

    #[error("triple {idx} (in {layer}) references unknown chunk provenance: {provenance}")]
    UnknownChunkProvenance {
        layer: String,
        idx: usize,
        provenance: String,
    },

    #[error("duplicate illustration id: {0}")]
    DuplicateIllustrationId(String),

    #[error("ontology entity '{entity}' has no triples (must have at least one)")]
    EmptyOntologyEntity { entity: String },

    #[error("cross-book anchor '{anchor}' has empty fires_when_reading list")]
    EmptyAnchorFiresList { anchor: String },

    #[error("scoring target out of [0.0, 1.0]: {field}={value}")]
    ScoringOutOfRange { field: String, value: f64 },

    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("JSON Schema validation failed: {message}")]
    JsonSchema { message: String },
}

// ─── Validators ─────────────────────────────────────────────────────────────

impl TutorRecord {
    /// Cross-field semantic validation that serde structural validation can't
    /// catch. Run AFTER successful deserialization. Examples:
    ///
    /// - schema_version equals [`SCHEMA_VERSION`]
    /// - chunk ids unique
    /// - illustration ids unique
    /// - LineRange has start ≤ end
    /// - triple provenance referencing a chunk_XXX id resolves to a real chunk
    /// - ontology entities have ≥ 1 triple
    /// - scoring fractions are in [0.0, 1.0]
    pub fn validate_semantics(&self) -> Result<(), ValidationError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ValidationError::SchemaVersionMismatch {
                expected: SCHEMA_VERSION.to_string(),
                got: self.schema_version.clone(),
            });
        }

        let chunk_ids = self.collect_chunk_ids()?;
        self.validate_layer_1(&chunk_ids)?;
        self.validate_layer_2()?;
        self.validate_layer_4()?;
        self.validate_layer_5()?;
        self.validate_scoring()?;

        Ok(())
    }

    fn collect_chunk_ids(&self) -> Result<std::collections::HashSet<String>, ValidationError> {
        let mut seen = std::collections::HashSet::new();
        for chunk in &self.layers.layer_1_literal.chunks {
            if let Some(line_range) = &chunk.source_lines
                && line_range.start > line_range.end
            {
                return Err(ValidationError::LineRangeInverted {
                    start: line_range.start,
                    end: line_range.end,
                });
            }
            if !seen.insert(chunk.id.clone()) {
                return Err(ValidationError::DuplicateChunkId(chunk.id.clone()));
            }
        }
        Ok(seen)
    }

    fn validate_layer_1(
        &self,
        chunk_ids: &std::collections::HashSet<String>,
    ) -> Result<(), ValidationError> {
        for (idx, triple) in self.layers.layer_1_literal.triples.iter().enumerate() {
            check_chunk_provenance(triple, chunk_ids, "layer_1_literal", idx)?;
        }
        Ok(())
    }

    fn validate_layer_2(&self) -> Result<(), ValidationError> {
        for entity in &self.layers.layer_2_ontology.entities {
            if entity.triples.is_empty() {
                return Err(ValidationError::EmptyOntologyEntity {
                    entity: entity.entity.clone(),
                });
            }
        }
        Ok(())
    }

    fn validate_layer_4(&self) -> Result<(), ValidationError> {
        for anchor in &self.layers.layer_4_cross_book.anchors {
            if anchor.fires_when_reading.is_empty() {
                return Err(ValidationError::EmptyAnchorFiresList {
                    anchor: anchor.anchor.clone(),
                });
            }
        }
        Ok(())
    }

    fn validate_layer_5(&self) -> Result<(), ValidationError> {
        let mut seen_illustrations = std::collections::HashSet::new();
        for ill in &self.layers.layer_5_multimodal.illustrations {
            if !seen_illustrations.insert(ill.id.clone()) {
                return Err(ValidationError::DuplicateIllustrationId(ill.id.clone()));
            }
        }
        Ok(())
    }

    fn validate_scoring(&self) -> Result<(), ValidationError> {
        let Some(scoring) = &self.scoring else {
            return Ok(());
        };
        for (name, val) in [
            ("recall_target", scoring.recall_target),
            ("precision_target", scoring.precision_target),
            ("cq_hit_target", scoring.cq_hit_target),
            ("gap_close_target", scoring.gap_close_target),
        ] {
            if let Some(v) = val
                && !(0.0..=1.0).contains(&v)
            {
                return Err(ValidationError::ScoringOutOfRange {
                    field: name.to_string(),
                    value: v,
                });
            }
        }
        Ok(())
    }
}

fn check_chunk_provenance(
    triple: &Triple,
    chunk_ids: &std::collections::HashSet<String>,
    layer: &str,
    idx: usize,
) -> Result<(), ValidationError> {
    let Some(prov) = &triple.provenance else {
        return Ok(());
    };
    // Only validate intra-document chunk references; external prefixes (wikidata:, conceptnet:, etc.)
    // are out of scope for this check.
    if prov.starts_with("chunk_") && !chunk_ids.contains(prov) {
        return Err(ValidationError::UnknownChunkProvenance {
            layer: layer.to_string(),
            idx,
            provenance: prov.clone(),
        });
    }
    Ok(())
}

// =====================================================================
// EX-4419 Phase 1 — Provenance four-tuple types
// =====================================================================
//
// Adds the audit-trail substrate Phase 6 (causal-gate refiner) needs.
// After this lands, every CQ in a Battery carries a four-tuple
// `(cq_id, requirement_id, scenario_id, tutor_seal)` so failure
// diagnosis can walk goal → requirement → CQ → match trail without
// re-running extraction by hand.
//
// Three types:
// - `Requirement` — one row of the requirements layer; bridges a being's
//   goal to the CQs that test it.
// - `TutorSeal` — immutable identity per CQ across plate revisions
//   (content_hash + author + timestamp + supersedes-chain).
// - `CqWithProvenance` — wraps an `OpenCq` with the four-tuple.
//
// Plus `compute_source_hash` for the plate-level `source_hash` field
// (separate from per-CQ TutorSeal — guards against the
// "plate authored against the wrong document" failure mode documented
// in `MATH_ACQUISITION_LIST.md`).

/// Bloom's taxonomy level. Mirrors `nusy_kbdd::BloomLevel` to avoid
/// circular crate dependencies — both crates need this enum but neither
/// should depend on the other for the schema layer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BloomLevel {
    Remember,
    Understand,
    Apply,
    Analyze,
    Evaluate,
    Create,
}

/// One requirement in the goal → requirement → CQ chain. A
/// `Requirement` is the bridge between a being's goal (loaded from
/// the kanban / KBDD scenarios) and the CQs that test whether the
/// being has met it.
///
/// Example: a `BeingGoal` like "the toddler-being can identify objects
/// in an alphabet primer" decomposes into requirements like
/// `REQ-001: identify Person entities`,
/// `REQ-002: identify Animal entities`, etc. Each requirement is then
/// tested by 1+ CQs.
///
/// Phase 5's invariant (and Phase 6's prerequisite): every CQ in a
/// graded Battery carries a `requirement_id` pointing back to a row
/// here, so causal-gate refinement (Phase 6) can validate that a
/// proposed refinement still terminates in the requirements layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    /// `REQ-XXX` ID. Stable across plate revisions for the same goal.
    pub id: String,
    /// Foreign-key to the originating goal in the kanban / scenario
    /// layer. Plain `String` to avoid coupling this schema crate to
    /// nusy-kbdd's internal goal types.
    pub goal_id: String,
    /// Human-readable description — what the requirement asserts.
    pub statement: String,
    /// Bloom level the requirement targets. Aligns CQ difficulty with
    /// the cognitive demand the goal expects.
    pub bloom_level: BloomLevel,
}

/// Immutable identity for a CQ across plate revisions. The triple of
/// `(content_hash, authored_at, author_agent)` uniquely names a
/// specific version of a CQ; `supersedes` walks the refinement chain
/// when a CQ is replaced (Phase 6's `plate_refiner` produces a new
/// `TutorSeal` whose `supersedes` points at the prior seal).
///
/// `content_hash` is `blake3` of `cq_text + scenarios + expected_keywords`
/// (canonicalized order, see [`TutorSeal::compute_content_hash`]).
/// Equivalent CQs from different runs hash to the same value; refined
/// CQs (different keywords or scenarios) hash differently and the
/// refinement carries the prior hash via `supersedes`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TutorSeal {
    /// `blake3` hash of the canonical CQ-content tuple. Hex-encoded
    /// for human-readable logs / kanban display; the binary form lives
    /// in `blake3::Hash::from_hex` round-tripping.
    pub content_hash: String,
    /// Unix milliseconds at which this seal was authored. Stable —
    /// re-authoring the same content produces the same `content_hash`
    /// but a new `authored_at`.
    pub authored_at: i64,
    /// Which agent authored this version. `"DGX"` / `"M5"` / `"Mini"`
    /// / `"Air"` / `"Captain"` / etc. Free-form string so future
    /// agents don't require a schema bump.
    pub author_agent: String,
    /// Prior seal if this CQ was refined from an earlier version.
    /// `Box` to avoid the recursive-type-of-infinite-size error;
    /// `None` is the natural base case for the original authoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Box<TutorSeal>>,
}

impl TutorSeal {
    /// Compute the canonical `content_hash` for a CQ. Inputs are
    /// canonicalized in fixed order so equivalent CQs produce
    /// identical hashes:
    ///
    /// ```text
    /// blake3(cq_text || "\n" || sorted(scenarios).join("\n") || "\n" || sorted(expected_keywords).join(","))
    /// ```
    ///
    /// `sorted()` is deterministic so `["b", "a"]` and `["a", "b"]`
    /// produce the same hash — keyword/scenario order is not
    /// load-bearing per the existing `derive_keywords` logic in
    /// `nusy-evaluator::battery`.
    pub fn compute_content_hash(
        cq_text: &str,
        scenarios: &[String],
        expected_keywords: &[String],
    ) -> String {
        let mut sorted_scenarios: Vec<&str> = scenarios.iter().map(String::as_str).collect();
        sorted_scenarios.sort_unstable();
        let mut sorted_keywords: Vec<&str> = expected_keywords.iter().map(String::as_str).collect();
        sorted_keywords.sort_unstable();

        let mut hasher = blake3::Hasher::new();
        hasher.update(cq_text.as_bytes());
        hasher.update(b"\n");
        hasher.update(sorted_scenarios.join("\n").as_bytes());
        hasher.update(b"\n");
        hasher.update(sorted_keywords.join(",").as_bytes());

        hasher.finalize().to_hex().to_string()
    }

    /// Walk the `supersedes` chain to build a vec of all prior seals,
    /// oldest-first. The current seal is NOT included — call site can
    /// prepend it if needed. Empty vec if this is the original
    /// authoring.
    ///
    /// Useful for `nk show CQ-XXX --history` (Phase 3) to display the
    /// refinement lineage.
    pub fn supersedes_chain(&self) -> Vec<&TutorSeal> {
        let mut out = Vec::new();
        let mut cursor = self.supersedes.as_deref();
        while let Some(seal) = cursor {
            out.push(seal);
            cursor = seal.supersedes.as_deref();
        }
        out.reverse();
        out
    }
}

/// CQ wrapped with the four-tuple provenance Phase 5 introduces:
/// `(cq_id, requirement_id, scenario_id, tutor_seal)`. The wrapped
/// `OpenCq` is the existing schema; the four extras are flat fields
/// so callers can read provenance without having to crack open the
/// inner CQ.
///
/// `requirement_id` is `Option` because the legacy `cq_battery.jsonl`
/// format and tier-1 retroactively-sealed entries don't have a
/// requirement layer yet. New (tier 4 / KBDD-decomposed) CQs always
/// carry one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CqWithProvenance {
    /// Stable CQ id (`CQ-XXX` / `cq_NNN`). Mirrors `OpenCq::id`.
    pub cq_id: String,
    /// Pointer into the requirements layer. `None` when the CQ was
    /// loaded from a pre-Phase-5 artifact (legacy `cq_battery.jsonl`,
    /// tier 1-3 plates that haven't been re-authored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirement_id: Option<String>,
    /// Pointer into the KBDD scenarios layer (Tier 4 / EX-4417). `None`
    /// for non-KBDD-decomposed CQs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    /// Immutable identity for THIS version of the CQ.
    pub tutor_seal: TutorSeal,
    /// The wrapped CQ — content, expected_resolution, etc. Round-trips
    /// through the existing `OpenCq` schema unchanged so legacy plates
    /// can be lifted to provenance-aware shape without losing fidelity.
    pub cq: OpenCq,
}

/// Compute `blake3` of source-document bytes. Used by the plate-level
/// `source_hash` guard: when a Battery is loaded for grading, the
/// grader recomputes this against the live source-doc bytes and rejects
/// the run with a `PlateSourceMismatch` if the hash differs from the
/// one stamped at plate-authoring time.
///
/// This guards against the "plate authored against the wrong document"
/// failure mode documented in `MATH_ACQUISITION_LIST.md` (e.g.
/// `pg40383_the_new_arithmetic.expected.json` was authored against
/// "Musical Myths and Facts" content, leading to silent grader
/// nonsense).
pub fn compute_source_hash(source_bytes: &[u8]) -> String {
    blake3::hash(source_bytes).to_hex().to_string()
}

/// Validate a JSON value against the embedded JSON Schema.
///
/// Use this when you receive a tutor record from an external source (CLI input,
/// HTTP request, file) and want a strong structural check before attempting
/// serde deserialization. For trusted inputs, [`TutorRecord::validate_semantics`]
/// after deserialize is sufficient.
pub fn validate_against_schema(json: &serde_json::Value) -> Result<(), ValidationError> {
    let schema_value: serde_json::Value =
        serde_json::from_str(SCHEMA_JSON).expect("embedded schema must parse");
    let validator =
        jsonschema::draft202012::new(&schema_value).map_err(|e| ValidationError::JsonSchema {
            message: format!("schema compile error: {e}"),
        })?;
    let errors: Vec<String> = validator.iter_errors(json).map(|e| e.to_string()).collect();
    if !errors.is_empty() {
        return Err(ValidationError::JsonSchema {
            message: errors.join("; "),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_record() -> TutorRecord {
        TutorRecord {
            schema_version: SCHEMA_VERSION.to_string(),
            document: DocumentRef {
                path: "test/doc.md".to_string(),
                level: Level::L0_toddler,
                title: None,
                era: None,
                audience: None,
                publisher: None,
                source_attribution: None,
            },
            tutor: TutorIdentity {
                name: "M5".to_string(),
                timestamp: DateTime::parse_from_rfc3339("2026-05-04T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                pipeline_lineage: None,
                captain_reframe: None,
            },
            gist: None,
            layers: Layers {
                layer_1_literal: Layer1Literal {
                    chunks: vec![],
                    triples: vec![],
                    target_count: None,
                },
                layer_2_ontology: Layer2Ontology {
                    entities: vec![],
                    target_count: None,
                },
                layer_3_curiosity: Layer3Curiosity {
                    cqs: vec![],
                    target_count: None,
                },
                layer_3_practice: None,
                layer_4_cross_book: Layer4CrossBook {
                    anchors: vec![],
                    target_count: None,
                },
                layer_5_multimodal: Layer5Multimodal {
                    illustrations: vec![],
                    cadence: None,
                    target_count: None,
                },
            },
            scoring: None,
            notes: None,
            source_hash: None,
            source_path: None,
            source_hash_note: None,
        }
    }

    #[test]
    fn minimal_record_validates() {
        assert!(minimal_record().validate_semantics().is_ok());
    }

    #[test]
    fn schema_version_mismatch_rejected() {
        let mut r = minimal_record();
        r.schema_version = "0.9".to_string();
        match r.validate_semantics() {
            Err(ValidationError::SchemaVersionMismatch { .. }) => {}
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_chunk_id_rejected() {
        let mut r = minimal_record();
        r.layers.layer_1_literal.chunks = vec![
            Y0Chunk {
                id: "chunk_001".to_string(),
                content: "a".to_string(),
                source_lines: None,
                role: None,
                salience: None,
            },
            Y0Chunk {
                id: "chunk_001".to_string(),
                content: "b".to_string(),
                source_lines: None,
                role: None,
                salience: None,
            },
        ];
        match r.validate_semantics() {
            Err(ValidationError::DuplicateChunkId(id)) => assert_eq!(id, "chunk_001"),
            other => panic!("expected DuplicateChunkId, got {other:?}"),
        }
    }

    #[test]
    fn line_range_inverted_rejected() {
        let mut r = minimal_record();
        r.layers.layer_1_literal.chunks = vec![Y0Chunk {
            id: "chunk_001".to_string(),
            content: "x".to_string(),
            source_lines: Some(LineRange { start: 50, end: 10 }),
            role: None,
            salience: None,
        }];
        match r.validate_semantics() {
            Err(ValidationError::LineRangeInverted { start, end }) => {
                assert_eq!(start, 50);
                assert_eq!(end, 10);
            }
            other => panic!("expected LineRangeInverted, got {other:?}"),
        }
    }

    #[test]
    fn unknown_chunk_provenance_rejected() {
        let mut r = minimal_record();
        r.layers.layer_1_literal.triples = vec![Triple {
            subject: "A".to_string(),
            predicate: "stands_for".to_string(),
            object: "Archer".to_string(),
            provenance: Some("chunk_999".to_string()),
            notes: None,
        }];
        match r.validate_semantics() {
            Err(ValidationError::UnknownChunkProvenance { provenance, .. }) => {
                assert_eq!(provenance, "chunk_999")
            }
            other => panic!("expected UnknownChunkProvenance, got {other:?}"),
        }
    }

    #[test]
    fn external_provenance_passes() {
        let mut r = minimal_record();
        r.layers.layer_2_ontology.entities = vec![OntologyEntity {
            entity: "Archer".to_string(),
            wikidata_id: Some("Q204339".to_string()),
            conceptnet_uri: None,
            triples: vec![Triple {
                subject: "Archer".to_string(),
                predicate: "rdf:type".to_string(),
                object: "Person".to_string(),
                provenance: Some("wikidata:Q204339".to_string()),
                notes: None,
            }],
        }];
        assert!(r.validate_semantics().is_ok());
    }

    #[test]
    fn empty_ontology_entity_rejected() {
        let mut r = minimal_record();
        r.layers.layer_2_ontology.entities = vec![OntologyEntity {
            entity: "Ghost".to_string(),
            wikidata_id: None,
            conceptnet_uri: None,
            triples: vec![],
        }];
        match r.validate_semantics() {
            Err(ValidationError::EmptyOntologyEntity { entity }) => assert_eq!(entity, "Ghost"),
            other => panic!("expected EmptyOntologyEntity, got {other:?}"),
        }
    }

    #[test]
    fn empty_anchor_fires_list_rejected() {
        let mut r = minimal_record();
        r.layers.layer_4_cross_book.anchors = vec![CrossBookAnchor {
            anchor: "Queen".to_string(),
            fires_when_reading: vec![],
            cross_link_to: None,
            notes: None,
        }];
        match r.validate_semantics() {
            Err(ValidationError::EmptyAnchorFiresList { anchor }) => assert_eq!(anchor, "Queen"),
            other => panic!("expected EmptyAnchorFiresList, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_illustration_id_rejected() {
        let mut r = minimal_record();
        r.layers.layer_5_multimodal.illustrations = vec![
            Illustration {
                id: "illustration_1".to_string(),
                location: None,
                expected_depicts: vec!["Archer".to_string()],
                expected_triples: None,
                cross_modal_anchor: None,
                notes: None,
            },
            Illustration {
                id: "illustration_1".to_string(),
                location: None,
                expected_depicts: vec!["Bow".to_string()],
                expected_triples: None,
                cross_modal_anchor: None,
                notes: None,
            },
        ];
        match r.validate_semantics() {
            Err(ValidationError::DuplicateIllustrationId(id)) => {
                assert_eq!(id, "illustration_1")
            }
            other => panic!("expected DuplicateIllustrationId, got {other:?}"),
        }
    }

    #[test]
    fn scoring_out_of_range_rejected() {
        let mut r = minimal_record();
        r.scoring = Some(Scoring {
            recall_target: Some(1.5),
            precision_target: None,
            cq_hit_target: None,
            gap_close_target: None,
            notes: None,
        });
        match r.validate_semantics() {
            Err(ValidationError::ScoringOutOfRange { field, value }) => {
                assert_eq!(field, "recall_target");
                assert_eq!(value, 1.5);
            }
            other => panic!("expected ScoringOutOfRange, got {other:?}"),
        }
    }

    #[test]
    fn cq_kind_serializes_snake_case() {
        let cq = OpenCq {
            id: None,
            cq: "What is an Archer?".to_string(),
            kind: CqKind::DirectWordMeaning,
            expected_resolution: None,
            expected_resolution_chain: None,
            expected_set: None,
            open: None,
            notes: None,
        };
        let json = serde_json::to_value(&cq).unwrap();
        assert_eq!(json["kind"], "direct_word_meaning");
    }

    #[test]
    fn salience_serializes_lowercase() {
        let chunk = Y0Chunk {
            id: "chunk_001".to_string(),
            content: "x".to_string(),
            source_lines: None,
            role: None,
            salience: Some(Salience::Skip),
        };
        let json = serde_json::to_value(&chunk).unwrap();
        assert_eq!(json["salience"], "skip");
    }

    #[test]
    fn schema_self_compiles() {
        let schema_value: serde_json::Value = serde_json::from_str(SCHEMA_JSON).unwrap();
        jsonschema::draft202012::new(&schema_value)
            .expect("embedded schema is a valid JSON Schema 2020-12 document");
    }

    #[test]
    fn schema_validates_minimal_record() {
        let r = minimal_record();
        let json = serde_json::to_value(&r).unwrap();
        validate_against_schema(&json).expect("minimal record passes JSON Schema");
    }

    #[test]
    fn schema_rejects_unknown_field() {
        let mut json = serde_json::to_value(minimal_record()).unwrap();
        json["unexpected_field"] = serde_json::json!("nope");
        match validate_against_schema(&json) {
            Err(ValidationError::JsonSchema { .. }) => {}
            other => panic!("expected JsonSchema error, got {other:?}"),
        }
    }

    // =================================================================
    // EX-4419 Phase 1 — Provenance four-tuple tests
    // =================================================================

    fn sample_seal() -> TutorSeal {
        TutorSeal {
            content_hash: TutorSeal::compute_content_hash(
                "What is an Archer?",
                &["scenario_a".to_string()],
                &["bow".to_string(), "arrow".to_string()],
            ),
            authored_at: 1_700_000_000_000,
            author_agent: "Mini".to_string(),
            supersedes: None,
        }
    }

    #[test]
    fn ex4419_tutor_seal_content_hash_is_deterministic() {
        let h1 = TutorSeal::compute_content_hash(
            "What is an Archer?",
            &["s1".to_string(), "s2".to_string()],
            &["bow".to_string(), "arrow".to_string()],
        );
        let h2 = TutorSeal::compute_content_hash(
            "What is an Archer?",
            &["s1".to_string(), "s2".to_string()],
            &["bow".to_string(), "arrow".to_string()],
        );
        assert_eq!(h1, h2, "same inputs must produce same hash");
        // 32-byte blake3 → 64 hex chars.
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn ex4419_tutor_seal_content_hash_is_order_independent() {
        // Scenarios and keywords are sets, not sequences.
        let h1 = TutorSeal::compute_content_hash(
            "What is an Archer?",
            &["s1".to_string(), "s2".to_string()],
            &["bow".to_string(), "arrow".to_string()],
        );
        let h2 = TutorSeal::compute_content_hash(
            "What is an Archer?",
            &["s2".to_string(), "s1".to_string()],
            &["arrow".to_string(), "bow".to_string()],
        );
        assert_eq!(h1, h2, "scenario / keyword order must not change hash");
    }

    #[test]
    fn ex4419_tutor_seal_content_hash_distinguishes_text() {
        let h1 = TutorSeal::compute_content_hash("question A", &[], &[]);
        let h2 = TutorSeal::compute_content_hash("question B", &[], &[]);
        assert_ne!(h1, h2, "different cq_text must produce different hash");
    }

    #[test]
    fn ex4419_tutor_seal_content_hash_distinguishes_keywords() {
        let h1 = TutorSeal::compute_content_hash("q", &[], &["a".to_string()]);
        let h2 = TutorSeal::compute_content_hash("q", &[], &["b".to_string()]);
        assert_ne!(h1, h2, "different keywords must produce different hash");
    }

    #[test]
    fn ex4419_tutor_seal_supersedes_chain_walks_oldest_first() {
        let v1 = TutorSeal {
            content_hash: "v1hash".to_string(),
            authored_at: 1000,
            author_agent: "M5".to_string(),
            supersedes: None,
        };
        let v2 = TutorSeal {
            content_hash: "v2hash".to_string(),
            authored_at: 2000,
            author_agent: "Mini".to_string(),
            supersedes: Some(Box::new(v1.clone())),
        };
        let v3 = TutorSeal {
            content_hash: "v3hash".to_string(),
            authored_at: 3000,
            author_agent: "DGX".to_string(),
            supersedes: Some(Box::new(v2.clone())),
        };
        let chain = v3.supersedes_chain();
        assert_eq!(chain.len(), 2);
        // Oldest first.
        assert_eq!(chain[0].content_hash, "v1hash");
        assert_eq!(chain[1].content_hash, "v2hash");
    }

    #[test]
    fn ex4419_tutor_seal_supersedes_chain_empty_for_original() {
        let original = sample_seal();
        assert!(original.supersedes_chain().is_empty());
    }

    #[test]
    fn ex4419_tutor_seal_round_trips_through_json() {
        let seal = TutorSeal {
            content_hash: "abc123".to_string(),
            authored_at: 1_700_000_000_000,
            author_agent: "Mini".to_string(),
            supersedes: Some(Box::new(TutorSeal {
                content_hash: "prior".to_string(),
                authored_at: 1_600_000_000_000,
                author_agent: "M5".to_string(),
                supersedes: None,
            })),
        };
        let json = serde_json::to_string(&seal).unwrap();
        let parsed: TutorSeal = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, seal);
    }

    #[test]
    fn ex4419_requirement_round_trips() {
        let req = Requirement {
            id: "REQ-001".to_string(),
            goal_id: "GOAL-toddler-identify-objects".to_string(),
            statement: "the being identifies Person entities in the primer".to_string(),
            bloom_level: BloomLevel::Understand,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: Requirement = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, req);
        assert!(json.contains("\"bloom_level\":\"understand\""));
    }

    #[test]
    fn ex4419_cq_with_provenance_round_trips() {
        let cqp = CqWithProvenance {
            cq_id: "CQ-001".to_string(),
            requirement_id: Some("REQ-001".to_string()),
            scenario_id: Some("SCEN-tier4-001".to_string()),
            tutor_seal: sample_seal(),
            cq: OpenCq {
                id: Some("CQ-001".to_string()),
                cq: "What is an Archer?".to_string(),
                kind: CqKind::DirectWordMeaning,
                expected_resolution: None,
                expected_resolution_chain: None,
                expected_set: None,
                open: None,
                notes: None,
            },
        };
        let json = serde_json::to_string(&cqp).unwrap();
        let parsed: CqWithProvenance = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cqp);
    }

    #[test]
    fn ex4419_cq_with_provenance_drops_optional_fields_when_none() {
        let cqp = CqWithProvenance {
            cq_id: "CQ-002".to_string(),
            requirement_id: None, // legacy CQ, no requirements layer
            scenario_id: None,    // not KBDD-decomposed
            tutor_seal: sample_seal(),
            cq: OpenCq {
                id: Some("CQ-002".to_string()),
                cq: "What is a Bow?".to_string(),
                kind: CqKind::DirectWordMeaning,
                expected_resolution: None,
                expected_resolution_chain: None,
                expected_set: None,
                open: None,
                notes: None,
            },
        };
        let json = serde_json::to_value(&cqp).unwrap();
        // Optional fields skipped when None.
        assert!(json.get("requirement_id").is_none());
        assert!(json.get("scenario_id").is_none());
    }

    #[test]
    fn ex4419_compute_source_hash_is_deterministic() {
        let bytes = b"the quick brown fox";
        let h1 = compute_source_hash(bytes);
        let h2 = compute_source_hash(bytes);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn ex4419_compute_source_hash_distinguishes_content() {
        let h1 = compute_source_hash(b"document A content");
        let h2 = compute_source_hash(b"document B content");
        assert_ne!(h1, h2);
    }

    #[test]
    fn ex4419_compute_source_hash_distinguishes_one_byte_change() {
        // The whole point of source_hash is catching wrong-doc errors.
        // A single-byte difference must change the hash.
        let h1 = compute_source_hash(b"document content");
        let h2 = compute_source_hash(b"Document content"); // capital D
        assert_ne!(h1, h2);
    }
}
