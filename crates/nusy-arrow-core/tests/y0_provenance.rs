//! Y0 Provenance integration tests — validates the ChunkTable schema,
//! the `source_chunk_id` FK on triples, and backward compatibility.
//!
//! These tests prove that:
//! 1. Chunks can be stored and queried via the ChunkTable schema
//! 2. Triples can reference chunks via `source_chunk_id`
//! 3. Triples without `source_chunk_id` fall back to `source_document`
//! 4. The FK relationship is navigable (triple → chunk → document)

use arrow::array::{
    Array, LargeStringArray, RecordBatch, StringArray, TimestampMillisecondArray, UInt8Array,
    UInt32Array, UInt64Array,
};
use nusy_arrow_core::{
    ArrowGraphStore, Namespace, QuerySpec, Triple, YLayer, chunk_col, chunks_schema, col,
};
use std::sync::Arc;

/// Build a sample ChunkTable RecordBatch with 5 chunks from a sample document.
fn sample_chunks_batch() -> RecordBatch {
    let schema = Arc::new(chunks_schema());
    let now_ms = chrono::Utc::now().timestamp_millis();

    let chunk_ids = vec![
        "chunk_woz_000",
        "chunk_woz_001",
        "chunk_woz_002",
        "chunk_woz_003",
        "chunk_woz_004",
    ];
    let doc_paths = vec!["wizard-of-oz.md"; 5];
    let contents: Vec<Option<&str>> = vec![
        Some("Dorothy lived in the midst of the great Kansas prairies."),
        Some("The Scarecrow found a Tin Woodman standing in the forest."),
        Some("Table 1: Characters and their desires.\n| Character | Desire |\n| Dorothy | Home |"),
        Some("'I shall take the heart,' returned the Tin Woodman."),
        Some("Figure 1: Map of the Land of Oz showing the Yellow Brick Road."),
    ];
    let token_counts: Vec<u32> = vec![12, 11, 18, 10, 14];
    let chunk_indices: Vec<u32> = (0..5).collect();
    let total_chunks: Vec<u32> = vec![5; 5];
    let char_starts: Vec<Option<u64>> = vec![Some(0), Some(58), Some(120), Some(210), Some(265)];
    let char_ends: Vec<Option<u64>> = vec![Some(57), Some(119), Some(209), Some(264), Some(330)];
    let page_numbers: Vec<Option<u32>> = vec![Some(1), Some(2), Some(3), Some(3), Some(4)];
    let section_headings: Vec<Option<&str>> = vec![
        Some("Chapter 1: The Cyclone"),
        Some("Chapter 4: The Road Through the Forest"),
        Some("Chapter 4: The Road Through the Forest"),
        Some("Chapter 5: The Rescue of the Tin Woodman"),
        Some("Appendix"),
    ];
    let section_levels: Vec<Option<u8>> = vec![Some(1), Some(1), Some(1), Some(1), Some(1)];
    let paragraph_indices: Vec<Option<u32>> = vec![Some(0), Some(3), None, Some(7), None];
    let element_types = vec!["prose", "prose", "table", "prose", "figure"];
    let namespaces = vec!["world"; 5];
    let y_layers: Vec<u8> = vec![0; 5];
    let extracted_bys: Vec<Option<&str>> = vec![Some("DGX"); 5];
    let timestamps: Vec<i64> = vec![now_ms; 5];

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(chunk_ids)),
            Arc::new(StringArray::from(doc_paths)),
            Arc::new(LargeStringArray::from(contents)),
            Arc::new(UInt32Array::from(token_counts)),
            Arc::new(UInt32Array::from(chunk_indices)),
            Arc::new(UInt32Array::from(total_chunks)),
            Arc::new(UInt64Array::from(char_starts)),
            Arc::new(UInt64Array::from(char_ends)),
            Arc::new(UInt32Array::from(page_numbers)),
            Arc::new(StringArray::from(section_headings)),
            Arc::new(UInt8Array::from(section_levels)),
            Arc::new(UInt32Array::from(paragraph_indices)),
            Arc::new(StringArray::from(element_types)),
            Arc::new(StringArray::from(namespaces)),
            Arc::new(UInt8Array::from(y_layers)),
            Arc::new(StringArray::from(extracted_bys)),
            Arc::new(TimestampMillisecondArray::from(timestamps).with_timezone("UTC")),
        ],
    )
    .expect("Failed to create chunks RecordBatch")
}

#[test]
fn test_chunks_schema_has_17_columns() {
    let schema = chunks_schema();
    assert_eq!(schema.fields().len(), 17);
    assert_eq!(schema.field(chunk_col::CHUNK_ID).name(), "chunk_id");
    assert_eq!(
        schema.field(chunk_col::DOCUMENT_PATH).name(),
        "document_path"
    );
    assert_eq!(schema.field(chunk_col::CONTENT).name(), "content");
    assert_eq!(schema.field(chunk_col::TOKEN_COUNT).name(), "token_count");
    assert_eq!(schema.field(chunk_col::CHUNK_INDEX).name(), "chunk_index");
    assert_eq!(schema.field(chunk_col::TOTAL_CHUNKS).name(), "total_chunks");
    assert_eq!(
        schema.field(chunk_col::CHAR_OFFSET_START).name(),
        "char_offset_start"
    );
    assert_eq!(
        schema.field(chunk_col::CHAR_OFFSET_END).name(),
        "char_offset_end"
    );
    assert_eq!(schema.field(chunk_col::PAGE_NUMBER).name(), "page_number");
    assert_eq!(
        schema.field(chunk_col::SECTION_HEADING).name(),
        "section_heading"
    );
    assert_eq!(
        schema.field(chunk_col::SECTION_LEVEL).name(),
        "section_level"
    );
    assert_eq!(
        schema.field(chunk_col::PARAGRAPH_INDEX).name(),
        "paragraph_index"
    );
    assert_eq!(schema.field(chunk_col::ELEMENT_TYPE).name(), "element_type");
    assert_eq!(schema.field(chunk_col::NAMESPACE).name(), "namespace");
    assert_eq!(schema.field(chunk_col::Y_LAYER).name(), "y_layer");
    assert_eq!(schema.field(chunk_col::EXTRACTED_BY).name(), "extracted_by");
    assert_eq!(schema.field(chunk_col::CREATED_AT).name(), "created_at");
}

#[test]
fn test_chunks_batch_round_trip() {
    let batch = sample_chunks_batch();
    assert_eq!(batch.num_rows(), 5);
    assert_eq!(batch.num_columns(), 17);

    // Verify element types
    let element_types = batch
        .column(chunk_col::ELEMENT_TYPE)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(element_types.value(0), "prose");
    assert_eq!(element_types.value(2), "table");
    assert_eq!(element_types.value(4), "figure");

    // Verify page numbers
    let pages = batch
        .column(chunk_col::PAGE_NUMBER)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .unwrap();
    assert_eq!(pages.value(0), 1);
    assert_eq!(pages.value(3), 3);

    // Verify content (LargeUtf8)
    let content = batch
        .column(chunk_col::CONTENT)
        .as_any()
        .downcast_ref::<LargeStringArray>()
        .unwrap();
    assert!(content.value(0).contains("Kansas prairies"));
}

#[test]
fn test_triples_schema_has_source_chunk_id() {
    let schema = nusy_arrow_core::schema::triples_schema();
    assert_eq!(schema.fields().len(), 19); // EX-4681 +object_datatype, EX-4682 +epistemic_status
    assert_eq!(schema.field(col::SOURCE_CHUNK_ID).name(), "source_chunk_id");
    // Verify it's nullable (triples may not have chunk-level provenance)
    assert!(schema.field(col::SOURCE_CHUNK_ID).is_nullable());
}

#[test]
fn test_symbolic_why_trace() {
    // This test validates the symbolic FK path:
    // triple.source_chunk_id → ChunkTable → document:section:paragraph
    let chunks = sample_chunks_batch();

    let mut store = ArrowGraphStore::new();

    // Create a triple referencing chunk_woz_001 (Chapter 4, paragraph 3)
    let triple = Triple {
        subject: "nusy:Scarecrow".to_string(),
        predicate: "nusy:found".to_string(),
        object: "nusy:TinWoodman".to_string(),
        graph: None,
        confidence: Some(0.95),
        source_document: Some("wizard-of-oz.md".to_string()),
        source_chunk_id: Some("chunk_woz_001".to_string()),
        extracted_by: Some("DGX".to_string()),
        caused_by: None,
        derived_from: None,
        consolidated_at: None,
        certifiability_class: None,
        object_datatype: None,
    };

    store
        .add_triple(&triple, Namespace::World, YLayer::Semantic)
        .unwrap();

    // Query the triple
    let results = store
        .query(&QuerySpec {
            subject: Some("nusy:Scarecrow".to_string()),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(results.len(), 1);
    let batch = &results[0];

    // Extract source_chunk_id from the triple
    let chunk_ids = batch
        .column(col::SOURCE_CHUNK_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert!(!chunk_ids.is_null(0));
    let chunk_id = chunk_ids.value(0);
    assert_eq!(chunk_id, "chunk_woz_001");

    // Symbolic WHY trace: look up the chunk in ChunkTable
    let chunk_id_col = chunks
        .column(chunk_col::CHUNK_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();

    // Find the matching chunk row
    let mut chunk_row = None;
    for i in 0..chunks.num_rows() {
        if chunk_id_col.value(i) == chunk_id {
            chunk_row = Some(i);
            break;
        }
    }
    let row = chunk_row.expect("Chunk should exist in ChunkTable");

    // Verify we can resolve to section/paragraph level
    let section = chunks
        .column(chunk_col::SECTION_HEADING)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(section.value(row), "Chapter 4: The Road Through the Forest");

    let paragraph = chunks
        .column(chunk_col::PARAGRAPH_INDEX)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .unwrap();
    assert_eq!(paragraph.value(row), 3);

    let page = chunks
        .column(chunk_col::PAGE_NUMBER)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .unwrap();
    assert_eq!(page.value(row), 2);

    let doc_path = chunks
        .column(chunk_col::DOCUMENT_PATH)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(doc_path.value(row), "wizard-of-oz.md");
}

#[test]
fn test_backward_compat_null_source_chunk_id() {
    // A triple with source_chunk_id = null should fall back to source_document
    let mut store = ArrowGraphStore::new();

    let triple = Triple {
        subject: "nusy:Dorothy".to_string(),
        predicate: "rdf:type".to_string(),
        object: "nusy:Character".to_string(),
        graph: None,
        confidence: Some(0.9),
        source_document: Some("wizard-of-oz.md".to_string()),
        source_chunk_id: None, // No chunk-level provenance
        extracted_by: Some("DGX".to_string()),
        caused_by: None,
        derived_from: None,
        consolidated_at: None,
        certifiability_class: None,
        object_datatype: None,
    };

    store
        .add_triple(&triple, Namespace::World, YLayer::Semantic)
        .unwrap();

    let results = store
        .query(&QuerySpec {
            subject: Some("nusy:Dorothy".to_string()),
            ..Default::default()
        })
        .unwrap();

    let batch = &results[0];

    // source_chunk_id should be null
    let chunk_ids = batch
        .column(col::SOURCE_CHUNK_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert!(chunk_ids.is_null(0));

    // But source_document should still be available
    let source_docs = batch
        .column(col::SOURCE_DOCUMENT)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert!(!source_docs.is_null(0));
    assert_eq!(source_docs.value(0), "wizard-of-oz.md");
}

#[test]
fn test_multiple_triples_same_chunk() {
    // Multiple triples can reference the same chunk (1:N relationship)
    let mut store = ArrowGraphStore::new();

    let triples = vec![
        Triple {
            subject: "nusy:Scarecrow".to_string(),
            predicate: "nusy:found".to_string(),
            object: "nusy:TinWoodman".to_string(),
            graph: None,
            confidence: Some(0.95),
            source_document: Some("wizard-of-oz.md".to_string()),
            source_chunk_id: Some("chunk_woz_001".to_string()),
            extracted_by: Some("DGX".to_string()),
            caused_by: None,
            derived_from: None,
            consolidated_at: None,
            certifiability_class: None,
            object_datatype: None,
        },
        Triple {
            subject: "nusy:TinWoodman".to_string(),
            predicate: "nusy:location".to_string(),
            object: "nusy:Forest".to_string(),
            graph: None,
            confidence: Some(0.90),
            source_document: Some("wizard-of-oz.md".to_string()),
            source_chunk_id: Some("chunk_woz_001".to_string()), // Same chunk
            extracted_by: Some("DGX".to_string()),
            caused_by: None,
            derived_from: None,
            consolidated_at: None,
            certifiability_class: None,
            object_datatype: None,
        },
        Triple {
            subject: "nusy:Dorothy".to_string(),
            predicate: "nusy:livesIn".to_string(),
            object: "nusy:Kansas".to_string(),
            graph: None,
            confidence: Some(0.99),
            source_document: Some("wizard-of-oz.md".to_string()),
            source_chunk_id: Some("chunk_woz_000".to_string()), // Different chunk
            extracted_by: Some("DGX".to_string()),
            caused_by: None,
            derived_from: None,
            consolidated_at: None,
            certifiability_class: None,
            object_datatype: None,
        },
    ];

    store
        .add_batch(&triples, Namespace::World, YLayer::Semantic)
        .unwrap();

    // All 3 triples should exist
    assert_eq!(store.len(), 3);

    // Query all triples — verify chunk references
    let results = store
        .query(&QuerySpec {
            namespace: Some(Namespace::World),
            ..Default::default()
        })
        .unwrap();
    let batch = &results[0];

    let chunk_ids = batch
        .column(col::SOURCE_CHUNK_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();

    // Count how many triples reference chunk_woz_001
    let mut chunk_001_count = 0;
    for i in 0..batch.num_rows() {
        if !chunk_ids.is_null(i) && chunk_ids.value(i) == "chunk_woz_001" {
            chunk_001_count += 1;
        }
    }
    assert_eq!(
        chunk_001_count, 2,
        "Two triples should reference chunk_woz_001"
    );
}
