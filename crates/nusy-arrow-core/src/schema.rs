//! Arrow schemas for the NuSy graph substrate.
//!
//! Three foundational tables:
//! - **Triples**: subject/predicate/object quads with provenance
//! - **Embeddings**: entity vectors (FixedSizeList<f32>)
//! - **Metadata**: per-entity access tracking and layer/namespace info

use arrow::array::RecordBatch;
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use std::sync::Arc;

/// Default embedding dimension for being-knowledge embeddings: 384, the output of
/// the production sentence models (all-MiniLM-L6-v2 / bge-small) and the dimension
/// `nusy-vector`'s HNSW index operates on. Unified at 384 (CH-4685) to remove the
/// arrow-core 768 vs nusy-vector 384 mismatch. Callers needing a different width
/// (e.g. bge-base 768 in nusy-graph-query, or code embeddings in nusy-codegraph)
/// use [`embeddings_schema_with_dim`] rather than this default.
pub const DEFAULT_EMBEDDING_DIM: i32 = 384;

/// Named column indices for the Triples schema.
/// Use these instead of hardcoded integers when accessing RecordBatch columns.
pub mod col {
    pub const TRIPLE_ID: usize = 0;
    pub const SUBJECT: usize = 1;
    pub const PREDICATE: usize = 2;
    pub const OBJECT: usize = 3;
    pub const GRAPH: usize = 4;
    pub const NAMESPACE: usize = 5;
    pub const Y_LAYER: usize = 6;
    pub const CONFIDENCE: usize = 7;
    pub const SOURCE_DOCUMENT: usize = 8;
    pub const SOURCE_CHUNK_ID: usize = 9;
    pub const EXTRACTED_BY: usize = 10;
    pub const CREATED_AT: usize = 11;
    pub const CAUSED_BY: usize = 12;
    pub const DERIVED_FROM: usize = 13;
    pub const CONSOLIDATED_AT: usize = 14;
    pub const DELETED: usize = 15;
    /// EX-3570: Certifiability class ("symbolic", "neural", "co-voted").
    pub const CERTIFIABILITY_CLASS: usize = 16;
    /// EX-4681: XSD datatype URI of the `object` literal (null = plain string).
    pub const OBJECT_DATATYPE: usize = 17;
    /// EX-4682: epistemic status — `asserted` (null/default) | `derived` | `believed`
    /// | `retracted`. `derived` is set only by governed engine write-back
    /// ([`crate::epistemic::promote_derived_fact`]); a null value reads as `asserted`.
    pub const EPISTEMIC_STATUS: usize = 18;
}

/// Named column indices for the Chunks schema (Y0 fine-grained provenance).
pub mod chunk_col {
    pub const CHUNK_ID: usize = 0;
    pub const DOCUMENT_PATH: usize = 1;
    pub const CONTENT: usize = 2;
    pub const TOKEN_COUNT: usize = 3;
    pub const CHUNK_INDEX: usize = 4;
    pub const TOTAL_CHUNKS: usize = 5;
    pub const CHAR_OFFSET_START: usize = 6;
    pub const CHAR_OFFSET_END: usize = 7;
    pub const PAGE_NUMBER: usize = 8;
    pub const SECTION_HEADING: usize = 9;
    pub const SECTION_LEVEL: usize = 10;
    pub const PARAGRAPH_INDEX: usize = 11;
    pub const ELEMENT_TYPE: usize = 12;
    pub const NAMESPACE: usize = 13;
    pub const Y_LAYER: usize = 14;
    pub const EXTRACTED_BY: usize = 15;
    pub const CREATED_AT: usize = 16;
}

/// Current schema version for the Triples table.
pub const TRIPLES_SCHEMA_VERSION: &str = "1.4.0";

/// Current schema version for the Chunks table.
pub const CHUNKS_SCHEMA_VERSION: &str = "1.0.0";

/// Schema for the Triples table — the core knowledge representation.
///
/// Columns (18 total, v1.3.0):
/// - `triple_id`: unique identifier (UUID string)
/// - `subject`, `predicate`, `object`: the RDF-like triple
/// - `graph`: named graph / context URI
/// - `namespace`: partition key (world/work/research/self)
/// - `y_layer`: Y-layer (0-6)
/// - `confidence`: float64 confidence score
/// - `source_document`: provenance document path
/// - `source_chunk_id`: FK to ChunkTable for fine-grained provenance (v1.1.0)
/// - `extracted_by`: agent/process that created this triple
/// - `created_at`: timestamp
/// - `caused_by`: triple_id of the triple that caused this one (causal chain)
/// - `derived_from`: triple_id of the triple this was derived from
/// - `consolidated_at`: timestamp when this triple was consolidated
/// - `deleted`: logical delete flag
pub fn triples_schema() -> Schema {
    Schema::new(vec![
        Field::new("triple_id", DataType::Utf8, false),
        Field::new("subject", DataType::Utf8, false),
        Field::new("predicate", DataType::Utf8, false),
        Field::new("object", DataType::Utf8, false),
        Field::new("graph", DataType::Utf8, true),
        Field::new("namespace", DataType::Utf8, false),
        Field::new("y_layer", DataType::UInt8, false),
        Field::new("confidence", DataType::Float64, true),
        Field::new("source_document", DataType::Utf8, true),
        Field::new("source_chunk_id", DataType::Utf8, true),
        Field::new("extracted_by", DataType::Utf8, true),
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new("caused_by", DataType::Utf8, true),
        Field::new("derived_from", DataType::Utf8, true),
        Field::new(
            "consolidated_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true,
        ),
        Field::new("deleted", DataType::Boolean, false),
        // EX-3570: Certifiability class for co-learning loop PAR tracking.
        Field::new("certifiability_class", DataType::Utf8, true),
        // EX-4681: XSD datatype URI of `object` (nullable; null = plain string literal).
        // `object` keeps its lexical form as Utf8 — this is a sidecar, never destructive.
        Field::new("object_datatype", DataType::Utf8, true),
        // EX-4682: epistemic status (nullable; null reads as `asserted`). Set to
        // `derived`/`believed`/`retracted` only by governed write-back, never at
        // construction time — so it is a schema column, not a `Triple` field.
        Field::new("epistemic_status", DataType::Utf8, true),
    ])
}

/// Schema for the Chunks table — Y0 fine-grained provenance.
///
/// Each row represents a chunk of a source document. Triples reference chunks
/// via `source_chunk_id` (FK). This enables WHY chains that resolve to
/// paragraph-level or finer granularity.
///
/// 17 columns total.
pub fn chunks_schema() -> Schema {
    Schema::new(vec![
        // Identity
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("document_path", DataType::Utf8, false),
        // Content
        Field::new("content", DataType::LargeUtf8, true),
        Field::new("token_count", DataType::UInt32, false),
        // Position within document
        Field::new("chunk_index", DataType::UInt32, false),
        Field::new("total_chunks", DataType::UInt32, false),
        Field::new("char_offset_start", DataType::UInt64, true),
        Field::new("char_offset_end", DataType::UInt64, true),
        // Structural metadata
        Field::new("page_number", DataType::UInt32, true),
        Field::new("section_heading", DataType::Utf8, true),
        Field::new("section_level", DataType::UInt8, true),
        Field::new("paragraph_index", DataType::UInt32, true),
        Field::new("element_type", DataType::Utf8, false),
        // Provenance
        Field::new("namespace", DataType::Utf8, false),
        Field::new("y_layer", DataType::UInt8, false),
        Field::new("extracted_by", DataType::Utf8, true),
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
    ])
}

// ── EX-4680 (V19 / VY-4679 E1): KnowledgeArtifacts + dependency tables ──────────
//
// FHIR-CPG / CRMI knowledge-artifact discipline, GENERALIZED (V19-VISION §4): a
// knowledge artifact is a versioned, lifecycle-managed unit (a rule set, a decision
// graph, an ontology fragment) with a canonical URL and a steward — never a clinical
// structure. The `graph` column of [`triples_schema`] is the artifact↔triples handle
// (a named graph keyed `canonical_url|version`); these two tables add the artifact
// metadata and the typed dependency edges over it. Lifecycle APIs, the manifest
// (transitive version-pinned closure), and persistence are added by later phases of
// EX-4680; this is the schema layer they build on.

/// Current schema version for the KnowledgeArtifacts table.
pub const KNOWLEDGE_ARTIFACTS_SCHEMA_VERSION: &str = "1.0.0";

/// Current schema version for the ArtifactDependencies table.
pub const ARTIFACT_DEPENDENCIES_SCHEMA_VERSION: &str = "1.0.0";

/// Named column indices for the KnowledgeArtifacts schema.
pub mod artifact_col {
    pub const ARTIFACT_ID: usize = 0;
    pub const ARTIFACT_TYPE: usize = 1;
    pub const VERSION: usize = 2;
    pub const STATUS: usize = 3;
    pub const CANONICAL_URL: usize = 4;
    pub const STEWARD: usize = 5;
    pub const DATE: usize = 6;
    pub const EFFECTIVE_START: usize = 7;
    pub const EFFECTIVE_END: usize = 8;
    pub const SUPERSEDES: usize = 9;
}

/// Named column indices for the ArtifactDependencies schema.
pub mod artifact_dep_col {
    pub const FROM_ARTIFACT: usize = 0;
    pub const TO_ARTIFACT: usize = 1;
    pub const DEP_TYPE: usize = 2;
}

/// Schema for the KnowledgeArtifacts table — versioned, lifecycle-managed units of
/// transferable knowledge (the generic form of a FHIR-CPG/CRMI knowledge artifact).
///
/// Columns (10 total, v1.0.0):
/// - `artifact_id`: stable identity across versions (the *business* identity)
/// - `artifact_type`: generic kind — `rule-set` | `decision-graph` | `ontology` | …
///   (the three north-star shapes; never a clinical resource type)
/// - `version`: business version `Major.Minor.Revision` (CRMI semantics)
/// - `status`: lifecycle — `draft` | `active` | `retired`. FHIR's `unknown` is
///   rejected or mapped at the import boundary, **never stored** here.
/// - `canonical_url`: stable URL identity; with `version` forms the named-graph handle
/// - `steward`: owning agent/org
/// - `date`: last-changed timestamp
/// - `effective_start` / `effective_end` (nullable): applicability window
/// - `supersedes` (nullable): the `artifact_id` this version replaces (the
///   defeasible-reasoning supersession edge)
pub fn knowledge_artifacts_schema() -> Schema {
    Schema::new(vec![
        Field::new("artifact_id", DataType::Utf8, false),
        Field::new("artifact_type", DataType::Utf8, false),
        Field::new("version", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("canonical_url", DataType::Utf8, false),
        Field::new("steward", DataType::Utf8, false),
        Field::new(
            "date",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new(
            "effective_start",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true,
        ),
        Field::new(
            "effective_end",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true,
        ),
        Field::new("supersedes", DataType::Utf8, true),
    ])
}

/// Schema for the ArtifactDependencies table — typed edges over knowledge artifacts.
///
/// Columns (3 total, v1.0.0):
/// - `from_artifact`, `to_artifact`: the dependency edge `from → to`
/// - `dep_type`: `depends-on` | `composed-of` | `derived-from` (the manifest closure
///   and COG-transfer packaging walk these edges)
pub fn artifact_dependencies_schema() -> Schema {
    Schema::new(vec![
        Field::new("from_artifact", DataType::Utf8, false),
        Field::new("to_artifact", DataType::Utf8, false),
        Field::new("dep_type", DataType::Utf8, false),
    ])
}

/// Normalize a RecordBatch from an older schema version to the current version.
///
/// This is the read-path migration: when loading Parquet files written with older
/// schemas, the normalizer adds missing columns with default values.
///
/// Supported migrations:
/// - v1.0.0 → v1.1.0: adds null `source_chunk_id` column at index 9
pub fn normalize_to_current(
    batch: &RecordBatch,
    from_version: &str,
) -> std::result::Result<RecordBatch, arrow::error::ArrowError> {
    use arrow::array::StringArray;

    // Trailing nullable columns appended since each version (all Utf8, all read as a
    // sensible default): certifiability_class (+1.2.0), object_datatype (+1.3.0),
    // epistemic_status (+1.4.0). `append_trailing_nulls(batch, k)` adds k null columns
    // and rebuilds against the current (19-col) schema.
    let append_trailing_nulls = |batch: &RecordBatch, k: usize| {
        let num_rows = batch.num_rows();
        let mut columns: Vec<Arc<dyn arrow::array::Array>> = Vec::with_capacity(19);
        for i in 0..batch.num_columns() {
            columns.push(batch.column(i).clone());
        }
        let nulls: Vec<Option<&str>> = vec![None; num_rows];
        for _ in 0..k {
            columns.push(Arc::new(StringArray::from(nulls.clone())));
        }
        RecordBatch::try_new(Arc::new(triples_schema()), columns)
    };

    match from_version {
        "1.4.0" => Ok(batch.clone()),
        // +epistemic_status
        "1.3.0" => append_trailing_nulls(batch, 1),
        // +object_datatype, +epistemic_status
        "1.2.0" => append_trailing_nulls(batch, 2),
        // +certifiability_class, +object_datatype, +epistemic_status
        "1.1.0" => append_trailing_nulls(batch, 3),
        "1.0.0" => {
            // v1.0.0 (15 cols) also inserts a null source_chunk_id at index 9, then the
            // three trailing nullable columns through to v1.4.0 (19 cols total).
            let num_rows = batch.num_rows();
            let nulls: Vec<Option<&str>> = vec![None; num_rows];
            let mut columns: Vec<Arc<dyn arrow::array::Array>> = Vec::with_capacity(19);
            for i in 0..9 {
                columns.push(batch.column(i).clone()); // triple_id..source_document
            }
            columns.push(Arc::new(StringArray::from(nulls.clone()))); // source_chunk_id at 9
            for i in 9..batch.num_columns() {
                columns.push(batch.column(i).clone()); // extracted_by..deleted
            }
            // certifiability_class + object_datatype + epistemic_status
            for _ in 0..3 {
                columns.push(Arc::new(StringArray::from(nulls.clone())));
            }
            RecordBatch::try_new(Arc::new(triples_schema()), columns)
        }
        other => Err(arrow::error::ArrowError::InvalidArgumentError(format!(
            "Unknown schema version '{}'. Supported: 1.0.0, 1.1.0, 1.2.0, 1.3.0, 1.4.0. \
             Upgrade nusy-arrow-core to read data from newer versions.",
            other
        ))),
    }
}

/// Schema for the Embeddings table — vector representations of entities.
pub fn embeddings_schema() -> Schema {
    embeddings_schema_with_dim(DEFAULT_EMBEDDING_DIM)
}

/// Embeddings schema with a custom vector dimension.
pub fn embeddings_schema_with_dim(dim: i32) -> Schema {
    Schema::new(vec![
        Field::new("entity_id", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, false)), dim),
            false,
        ),
    ])
}

/// Schema for the Metadata table — per-entity access tracking.
pub fn metadata_schema() -> Schema {
    Schema::new(vec![
        Field::new("entity_id", DataType::Utf8, false),
        Field::new("y_layer", DataType::UInt8, false),
        Field::new("namespace", DataType::Utf8, false),
        Field::new("access_count", DataType::UInt64, false),
        Field::new(
            "last_accessed",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true,
        ),
    ])
}

// ── EX-4682 (V19 / VY-4679 E3): Evidence/certainty table ─────────────────────
//
// FHIR R5 Evidence.certainty (recursive, rating-system-agnostic) generalized: a
// certainty rating attaches to a knowledge artifact, has a type (Overall, RiskOfBias,
// Imprecision, …, open vocab) and a rating under some rating *system* (GRADE is one
// instance among many — a value, not an enum), and may have sub-component ratings
// (`parent_certainty_id`) so an Overall rating decomposes into its GRADE domains.

/// Current schema version for the Certainty table.
pub const CERTAINTY_SCHEMA_VERSION: &str = "1.0.0";

/// Named column indices for the Certainty schema.
pub mod certainty_col {
    pub const CERTAINTY_ID: usize = 0;
    pub const ARTIFACT_ID: usize = 1;
    pub const CERTAINTY_TYPE: usize = 2;
    pub const RATING: usize = 3;
    pub const RATER: usize = 4;
    pub const RATING_SYSTEM: usize = 5;
    pub const DIRECTNESS: usize = 6;
    pub const PARENT_CERTAINTY_ID: usize = 7;
}

/// Schema for the Certainty table — recursive, rating-system-agnostic evidence grading
/// over knowledge artifacts (EX-4682). `rating_system` is data (e.g. `"GRADE"`), never an
/// enum; `parent_certainty_id` (nullable) links a sub-component to its overall rating;
/// `directness` is the COG context-match axis (`low|moderate|high|exact`).
pub fn certainty_schema() -> Schema {
    Schema::new(vec![
        Field::new("certainty_id", DataType::Utf8, false),
        Field::new("artifact_id", DataType::Utf8, false),
        Field::new("certainty_type", DataType::Utf8, false),
        Field::new("rating", DataType::Utf8, false),
        Field::new("rater", DataType::Utf8, true),
        Field::new("rating_system", DataType::Utf8, false),
        Field::new("directness", DataType::Utf8, true),
        Field::new("parent_certainty_id", DataType::Utf8, true),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{
        Array, BooleanArray, Float64Array, RecordBatch, StringArray, TimestampMillisecondArray,
        UInt8Array,
    };

    #[test]
    fn test_triples_schema_creates_record_batch() {
        let schema = Arc::new(triples_schema());
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["t-001"])),
                Arc::new(StringArray::from(vec!["nusy:Santiago"])),
                Arc::new(StringArray::from(vec!["rdf:type"])),
                Arc::new(StringArray::from(vec!["nusy:Being"])),
                Arc::new(StringArray::from(vec![Some("default")])),
                Arc::new(StringArray::from(vec!["world"])),
                Arc::new(UInt8Array::from(vec![1u8])),
                Arc::new(Float64Array::from(vec![Some(0.95)])),
                Arc::new(StringArray::from(vec![Some("ontology.md")])),
                Arc::new(StringArray::from(vec![Some("chunk_onto_001")])), // source_chunk_id
                Arc::new(StringArray::from(vec![Some("DGX")])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![Some("t-000")])), // caused_by
                Arc::new(StringArray::from(vec![Some("t-base")])), // derived_from
                Arc::new(TimestampMillisecondArray::from(vec![Some(now_ms)]).with_timezone("UTC")), // consolidated_at
                Arc::new(BooleanArray::from(vec![false])),
                Arc::new(StringArray::from(vec![None::<&str>])), // certifiability_class
                Arc::new(StringArray::from(vec![Some(
                    "http://www.w3.org/2001/XMLSchema#string",
                )])), // object_datatype
                Arc::new(StringArray::from(vec![None::<&str>])), // epistemic_status
            ],
        )
        .expect("Failed to create triples RecordBatch");

        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 19);
    }

    #[test]
    fn test_chunks_schema_creates_record_batch() {
        use arrow::array::{LargeStringArray, UInt32Array, UInt64Array};

        let schema = Arc::new(chunks_schema());
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["chunk_woz_001"])),
                Arc::new(StringArray::from(vec!["wizard-of-oz.md"])),
                Arc::new(LargeStringArray::from(vec![Some(
                    "Dorothy carried the shoes...",
                )])),
                Arc::new(UInt32Array::from(vec![42u32])),
                Arc::new(UInt32Array::from(vec![0u32])),
                Arc::new(UInt32Array::from(vec![10u32])),
                Arc::new(UInt64Array::from(vec![Some(0u64)])),
                Arc::new(UInt64Array::from(vec![Some(156u64)])),
                Arc::new(UInt32Array::from(vec![Some(36u32)])),
                Arc::new(StringArray::from(vec![Some("Chapter 2: The Council")])),
                Arc::new(UInt8Array::from(vec![Some(2u8)])),
                Arc::new(UInt32Array::from(vec![Some(7u32)])),
                Arc::new(StringArray::from(vec!["prose"])),
                Arc::new(StringArray::from(vec!["world"])),
                Arc::new(UInt8Array::from(vec![0u8])),
                Arc::new(StringArray::from(vec![Some("DGX")])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
            ],
        )
        .expect("Failed to create chunks RecordBatch");

        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 17);
    }

    #[test]
    fn test_normalize_v1_0_0_to_v1_1_0() {
        // Build a v1.0.0 batch (15 columns, no source_chunk_id)
        let v1_0_schema = Arc::new(Schema::new(vec![
            Field::new("triple_id", DataType::Utf8, false),
            Field::new("subject", DataType::Utf8, false),
            Field::new("predicate", DataType::Utf8, false),
            Field::new("object", DataType::Utf8, false),
            Field::new("graph", DataType::Utf8, true),
            Field::new("namespace", DataType::Utf8, false),
            Field::new("y_layer", DataType::UInt8, false),
            Field::new("confidence", DataType::Float64, true),
            Field::new("source_document", DataType::Utf8, true),
            Field::new("extracted_by", DataType::Utf8, true),
            Field::new(
                "created_at",
                DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
                false,
            ),
            Field::new("caused_by", DataType::Utf8, true),
            Field::new("derived_from", DataType::Utf8, true),
            Field::new(
                "consolidated_at",
                DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
                true,
            ),
            Field::new("deleted", DataType::Boolean, false),
        ]));
        let now_ms = chrono::Utc::now().timestamp_millis();

        let old_batch = RecordBatch::try_new(
            v1_0_schema,
            vec![
                Arc::new(StringArray::from(vec!["t-001"])),
                Arc::new(StringArray::from(vec!["sub"])),
                Arc::new(StringArray::from(vec!["pred"])),
                Arc::new(StringArray::from(vec!["obj"])),
                Arc::new(StringArray::from(vec![Some("default")])),
                Arc::new(StringArray::from(vec!["world"])),
                Arc::new(UInt8Array::from(vec![1u8])),
                Arc::new(Float64Array::from(vec![Some(0.9)])),
                Arc::new(StringArray::from(vec![Some("doc.md")])),
                Arc::new(StringArray::from(vec![Some("DGX")])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![None::<&str>])),
                Arc::new(StringArray::from(vec![None::<&str>])),
                Arc::new(TimestampMillisecondArray::from(vec![None]).with_timezone("UTC")),
                Arc::new(BooleanArray::from(vec![false])),
            ],
        )
        .unwrap();

        assert_eq!(old_batch.num_columns(), 15);

        let normalized = normalize_to_current(&old_batch, "1.0.0").unwrap();
        assert_eq!(normalized.num_columns(), 19);
        assert_eq!(normalized.schema(), Arc::new(triples_schema()));

        // source_chunk_id (index 9) should be null
        let chunk_id_col = normalized
            .column(col::SOURCE_CHUNK_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(chunk_id_col.is_null(0));

        // extracted_by (now index 10) should be preserved
        let extracted = normalized
            .column(col::EXTRACTED_BY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(extracted.value(0), "DGX");
    }

    #[test]
    fn test_normalize_v1_1_0_adds_certifiability_class() {
        // Build a v1.1.0 batch (16 columns, no certifiability_class)
        let v1_1_schema = Arc::new(Schema::new(vec![
            Field::new("triple_id", DataType::Utf8, false),
            Field::new("subject", DataType::Utf8, false),
            Field::new("predicate", DataType::Utf8, false),
            Field::new("object", DataType::Utf8, false),
            Field::new("graph", DataType::Utf8, true),
            Field::new("namespace", DataType::Utf8, false),
            Field::new("y_layer", DataType::UInt8, false),
            Field::new("confidence", DataType::Float64, true),
            Field::new("source_document", DataType::Utf8, true),
            Field::new("source_chunk_id", DataType::Utf8, true),
            Field::new("extracted_by", DataType::Utf8, true),
            Field::new(
                "created_at",
                DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
                false,
            ),
            Field::new("caused_by", DataType::Utf8, true),
            Field::new("derived_from", DataType::Utf8, true),
            Field::new(
                "consolidated_at",
                DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
                true,
            ),
            Field::new("deleted", DataType::Boolean, false),
        ]));
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            v1_1_schema,
            vec![
                Arc::new(StringArray::from(vec!["t-001"])),
                Arc::new(StringArray::from(vec!["sub"])),
                Arc::new(StringArray::from(vec!["pred"])),
                Arc::new(StringArray::from(vec!["obj"])),
                Arc::new(StringArray::from(vec![Some("default")])),
                Arc::new(StringArray::from(vec!["world"])),
                Arc::new(UInt8Array::from(vec![1u8])),
                Arc::new(Float64Array::from(vec![Some(0.9)])),
                Arc::new(StringArray::from(vec![Some("doc.md")])),
                Arc::new(StringArray::from(vec![None::<&str>])), // source_chunk_id
                Arc::new(StringArray::from(vec![Some("DGX")])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![None::<&str>])),
                Arc::new(StringArray::from(vec![None::<&str>])),
                Arc::new(TimestampMillisecondArray::from(vec![None]).with_timezone("UTC")),
                Arc::new(BooleanArray::from(vec![false])),
            ],
        )
        .unwrap();

        assert_eq!(batch.num_columns(), 16);

        let normalized = normalize_to_current(&batch, "1.1.0").unwrap();
        assert_eq!(normalized.num_columns(), 19);
        assert_eq!(normalized.schema(), Arc::new(triples_schema()));

        // certifiability_class (col 16) should be null
        let cert_col = normalized
            .column(col::CERTIFIABILITY_CLASS)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(cert_col.is_null(0));
    }

    #[test]
    fn test_normalize_unknown_version_errors() {
        let schema = Arc::new(triples_schema());
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["t-001"])),
                Arc::new(StringArray::from(vec!["sub"])),
                Arc::new(StringArray::from(vec!["pred"])),
                Arc::new(StringArray::from(vec!["obj"])),
                Arc::new(StringArray::from(vec![Some("default")])),
                Arc::new(StringArray::from(vec!["world"])),
                Arc::new(UInt8Array::from(vec![1u8])),
                Arc::new(Float64Array::from(vec![Some(0.9)])),
                Arc::new(StringArray::from(vec![Some("doc.md")])),
                Arc::new(StringArray::from(vec![None::<&str>])),
                Arc::new(StringArray::from(vec![Some("DGX")])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![None::<&str>])),
                Arc::new(StringArray::from(vec![None::<&str>])),
                Arc::new(TimestampMillisecondArray::from(vec![None]).with_timezone("UTC")),
                Arc::new(BooleanArray::from(vec![false])),
                Arc::new(StringArray::from(vec![None::<&str>])), // certifiability_class
                Arc::new(StringArray::from(vec![None::<&str>])), // object_datatype
                Arc::new(StringArray::from(vec![None::<&str>])), // epistemic_status
            ],
        )
        .unwrap();

        let result = normalize_to_current(&batch, "2.0.0");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Unknown schema version"));
    }

    #[test]
    fn test_embeddings_schema_creates_record_batch() {
        use arrow::array::{FixedSizeListArray, Float32Array};

        let schema = Arc::new(embeddings_schema_with_dim(4));
        let values = Float32Array::from(vec![0.1, 0.2, 0.3, 0.4]);
        let list = FixedSizeListArray::try_new(
            Arc::new(Field::new("item", DataType::Float32, false)),
            4,
            Arc::new(values),
            None,
        )
        .unwrap();

        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec!["e-001"])), Arc::new(list)],
        )
        .expect("Failed to create embeddings RecordBatch");

        assert_eq!(batch.num_rows(), 1);
    }

    #[test]
    fn test_metadata_schema_creates_record_batch() {
        use arrow::array::UInt64Array;

        let schema = Arc::new(metadata_schema());
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["e-001"])),
                Arc::new(UInt8Array::from(vec![2u8])),
                Arc::new(StringArray::from(vec!["work"])),
                Arc::new(UInt64Array::from(vec![42u64])),
                Arc::new(
                    TimestampMillisecondArray::from(vec![Some(
                        chrono::Utc::now().timestamp_millis(),
                    )])
                    .with_timezone("UTC"),
                ),
            ],
        )
        .expect("Failed to create metadata RecordBatch");

        assert_eq!(batch.num_rows(), 1);
    }

    // ── EX-4680: KnowledgeArtifacts + dependency tables ──────────────────────

    #[test]
    fn knowledge_artifacts_schema_shape_and_nullability() {
        let s = knowledge_artifacts_schema();
        let names: Vec<&str> = s.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            names,
            vec![
                "artifact_id",
                "artifact_type",
                "version",
                "status",
                "canonical_url",
                "steward",
                "date",
                "effective_start",
                "effective_end",
                "supersedes",
            ]
        );
        // Identity/version/status/url/steward/date are required; the applicability
        // window and the supersession edge are optional.
        for required in [
            "artifact_id",
            "artifact_type",
            "version",
            "status",
            "canonical_url",
            "steward",
            "date",
        ] {
            assert!(
                !s.field_with_name(required).unwrap().is_nullable(),
                "{required} must be non-nullable"
            );
        }
        for optional in ["effective_start", "effective_end", "supersedes"] {
            assert!(
                s.field_with_name(optional).unwrap().is_nullable(),
                "{optional} must be nullable"
            );
        }
    }

    #[test]
    fn artifact_dependencies_schema_shape() {
        let s = artifact_dependencies_schema();
        let names: Vec<&str> = s.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(names, vec!["from_artifact", "to_artifact", "dep_type"]);
        for f in s.fields() {
            assert!(!f.is_nullable(), "{} must be non-nullable", f.name());
        }
    }

    #[test]
    fn artifact_col_indices_align_with_schema() {
        let s = knowledge_artifacts_schema();
        assert_eq!(s.field(artifact_col::ARTIFACT_ID).name(), "artifact_id");
        assert_eq!(s.field(artifact_col::STATUS).name(), "status");
        assert_eq!(s.field(artifact_col::SUPERSEDES).name(), "supersedes");
        let d = artifact_dependencies_schema();
        assert_eq!(
            d.field(artifact_dep_col::FROM_ARTIFACT).name(),
            "from_artifact"
        );
        assert_eq!(d.field(artifact_dep_col::DEP_TYPE).name(), "dep_type");
    }

    #[test]
    fn knowledge_artifacts_schema_builds_a_record_batch() {
        let schema = Arc::new(knowledge_artifacts_schema());
        let now_ms = chrono::Utc::now().timestamp_millis();
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["art-1"])),
                Arc::new(StringArray::from(vec!["rule-set"])),
                Arc::new(StringArray::from(vec!["1.0.0"])),
                Arc::new(StringArray::from(vec!["active"])),
                Arc::new(StringArray::from(vec!["https://nusy.dev/ka/art-1"])),
                Arc::new(StringArray::from(vec!["Air"])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(TimestampMillisecondArray::from(vec![None::<i64>]).with_timezone("UTC")),
                Arc::new(TimestampMillisecondArray::from(vec![None::<i64>]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![None::<&str>])),
            ],
        )
        .expect("KnowledgeArtifacts RecordBatch builds against its schema");
        assert_eq!(batch.num_rows(), 1);
    }
}
