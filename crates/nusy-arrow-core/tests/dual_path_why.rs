//! Dual-Path WHY Chain integration test.
//!
//! Validates BOTH WHY resolution paths from CHORE-158 architecture:
//! - **Symbolic path:** triple.source_chunk_id → ChunkTable → document:section:paragraph (exact)
//! - **Neural path:** entity embedding → k-nearest chunk embeddings (associative)
//!
//! This is the H-Y0-1 hypothesis gate:
//! do(define fine-grained Y0 schema) → WHY provenance chains resolve to paragraph level
//! via BOTH symbolic and neural paths.

use arrow::array::{
    Array, FixedSizeListArray, Float32Array, LargeStringArray, RecordBatch, StringArray,
    TimestampMillisecondArray, UInt8Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::{DataType, Field};
use nusy_arrow_core::{
    ArrowGraphStore, Namespace, QuerySpec, Triple, YLayer, chunk_col, chunks_schema, col,
    embeddings_schema_with_dim,
};
use std::sync::Arc;

const EMBED_DIM: i32 = 4; // Small dimension for testing

/// Build chunks from a sample medical guideline document.
fn medical_chunks_batch() -> RecordBatch {
    let schema = Arc::new(chunks_schema());
    let now_ms = chrono::Utc::now().timestamp_millis();

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![
                "chunk_cpg_001",
                "chunk_cpg_002",
                "chunk_cpg_003",
                "chunk_cpg_004",
                "chunk_cpg_005",
            ])),
            Arc::new(StringArray::from(vec!["ada-guidelines.pdf"; 5])),
            Arc::new(LargeStringArray::from(vec![
                Some("Metformin should be the initial pharmacologic agent for type 2 diabetes."),
                Some("Table 9.1: First-line medications for T2DM.\n| Drug | Class | Evidence |\n| Metformin | Biguanide | A |"),
                Some("For patients with established ASCVD, GLP-1 RA or SGLT2i with proven benefit is recommended."),
                Some("Insulin therapy should be considered when A1C is above 10% at diagnosis."),
                Some("Lifestyle interventions including diet and exercise remain the foundation of diabetes management."),
            ])),
            Arc::new(UInt32Array::from(vec![14u32, 22, 18, 15, 13])),
            Arc::new(UInt32Array::from(vec![0u32, 1, 2, 3, 4])),
            Arc::new(UInt32Array::from(vec![5u32; 5])),
            Arc::new(UInt64Array::from(vec![Some(0u64), Some(75), Some(180), Some(290), Some(370)])),
            Arc::new(UInt64Array::from(vec![Some(74u64), Some(179), Some(289), Some(369), Some(450)])),
            Arc::new(UInt32Array::from(vec![Some(36u32), Some(37), Some(38), Some(40), Some(12)])),
            Arc::new(StringArray::from(vec![
                Some("Section 9.2: Pharmacologic Treatment"),
                Some("Section 9.2: Pharmacologic Treatment"),
                Some("Section 9.3: Cardiovascular Risk"),
                Some("Section 9.4: Insulin Therapy"),
                Some("Section 4: Lifestyle Management"),
            ])),
            Arc::new(UInt8Array::from(vec![Some(2u8); 5])),
            Arc::new(UInt32Array::from(vec![Some(4u32), None, Some(1), Some(2), Some(0)])),
            Arc::new(StringArray::from(vec!["prose", "table", "prose", "prose", "prose"])),
            Arc::new(StringArray::from(vec!["world"; 5])),
            Arc::new(UInt8Array::from(vec![0u8; 5])),
            Arc::new(StringArray::from(vec![Some("DGX"); 5])),
            Arc::new(TimestampMillisecondArray::from(vec![now_ms; 5]).with_timezone("UTC")),
        ],
    )
    .expect("Failed to create medical chunks batch")
}

/// Build chunk embeddings in the EmbeddingsTable (entity_id = chunk_id).
/// Uses 4-dimensional vectors for testing (real system uses 384, the MiniLM/bge-small output).
fn chunk_embeddings_batch() -> RecordBatch {
    let schema = Arc::new(embeddings_schema_with_dim(EMBED_DIM));

    // Embeddings designed so that:
    // - chunk_cpg_001 (metformin) and chunk_cpg_002 (medication table) are similar
    // - chunk_cpg_003 (cardiovascular) is moderately related
    // - chunk_cpg_005 (lifestyle) is distant
    let entity_ids = vec![
        "chunk_cpg_001",
        "chunk_cpg_002",
        "chunk_cpg_003",
        "chunk_cpg_004",
        "chunk_cpg_005",
    ];

    let flat_vectors: Vec<f32> = vec![
        // chunk_cpg_001: metformin pharmacologic
        0.9, 0.8, 0.1, 0.2, // chunk_cpg_002: medication table (similar to 001)
        0.85, 0.75, 0.15, 0.25, // chunk_cpg_003: cardiovascular (moderately related)
        0.5, 0.6, 0.7, 0.3, // chunk_cpg_004: insulin therapy (related to medication)
        0.7, 0.65, 0.3, 0.4, // chunk_cpg_005: lifestyle (distant)
        0.1, 0.2, 0.9, 0.8,
    ];

    let values = Float32Array::from(flat_vectors);
    let list = FixedSizeListArray::try_new(
        Arc::new(Field::new("item", DataType::Float32, false)),
        EMBED_DIM,
        Arc::new(values),
        None,
    )
    .unwrap();

    RecordBatch::try_new(
        schema,
        vec![Arc::new(StringArray::from(entity_ids)), Arc::new(list)],
    )
    .expect("Failed to create embeddings batch")
}

/// Cosine similarity between two f32 slices.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[test]
fn test_symbolic_why_path_resolves_to_paragraph() {
    let chunks = medical_chunks_batch();
    let mut store = ArrowGraphStore::new();

    // Create a triple about metformin recommendation, linked to chunk_cpg_001
    let triple = Triple {
        subject: "nusy:Patient".to_string(),
        predicate: "nusy:recommended".to_string(),
        object: "nusy:Metformin".to_string(),
        graph: None,
        confidence: Some(0.95),
        source_document: Some("ada-guidelines.pdf".to_string()),
        source_chunk_id: Some("chunk_cpg_001".to_string()),
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

    // Step 1: Query the triple
    let results = store
        .query(&QuerySpec {
            subject: Some("nusy:Patient".to_string()),
            predicate: Some("nusy:recommended".to_string()),
            ..Default::default()
        })
        .unwrap();

    let batch = &results[0];
    let chunk_id = batch
        .column(col::SOURCE_CHUNK_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
        .value(0);

    // Step 2: Symbolic trace — follow FK to ChunkTable
    let chunk_ids_col = chunks
        .column(chunk_col::CHUNK_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();

    let row = (0..chunks.num_rows())
        .find(|&i| chunk_ids_col.value(i) == chunk_id)
        .expect("Chunk must exist");

    // Step 3: Verify paragraph-level resolution
    let doc = chunks
        .column(chunk_col::DOCUMENT_PATH)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
        .value(row);
    let section = chunks
        .column(chunk_col::SECTION_HEADING)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
        .value(row);
    let page = chunks
        .column(chunk_col::PAGE_NUMBER)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .unwrap()
        .value(row);
    let paragraph = chunks
        .column(chunk_col::PARAGRAPH_INDEX)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .unwrap()
        .value(row);
    let content = chunks
        .column(chunk_col::CONTENT)
        .as_any()
        .downcast_ref::<LargeStringArray>()
        .unwrap()
        .value(row);

    // H-Y0-1 gate: WHY chain resolves to paragraph level
    assert_eq!(doc, "ada-guidelines.pdf");
    assert_eq!(section, "Section 9.2: Pharmacologic Treatment");
    assert_eq!(page, 36);
    assert_eq!(paragraph, 4);
    assert!(content.contains("Metformin should be the initial pharmacologic agent"));
}

#[test]
fn test_neural_why_path_returns_semantically_related_chunks() {
    let embeddings = chunk_embeddings_batch();

    // Simulate a query: "Why recommend metformin?"
    // The entity embedding for the metformin concept is similar to chunk_cpg_001
    let query_embedding: Vec<f32> = vec![0.88, 0.78, 0.12, 0.22];

    // Extract all chunk embeddings and compute similarities
    let entity_ids = embeddings
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let vectors = embeddings
        .column(1)
        .as_any()
        .downcast_ref::<FixedSizeListArray>()
        .unwrap();

    let mut similarities: Vec<(String, f32)> = Vec::new();

    for i in 0..embeddings.num_rows() {
        let chunk_id = entity_ids.value(i).to_string();
        let vec_values = vectors
            .value(i)
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap()
            .values()
            .to_vec();
        let sim = cosine_similarity(&query_embedding, &vec_values);
        similarities.push((chunk_id, sim));
    }

    // Sort by similarity (descending)
    similarities.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    // Neural path should return chunk_cpg_001 (metformin) as most similar
    assert_eq!(
        similarities[0].0, "chunk_cpg_001",
        "Most similar chunk should be the metformin chunk"
    );

    // chunk_cpg_002 (medication table) should be second most similar
    assert_eq!(
        similarities[1].0, "chunk_cpg_002",
        "Second most similar should be the medication table"
    );

    // chunk_cpg_005 (lifestyle) should be least similar
    assert_eq!(
        similarities[4].0, "chunk_cpg_005",
        "Least similar should be the lifestyle chunk"
    );

    // Top result similarity should be high (>0.99 for our test vectors)
    assert!(
        similarities[0].1 > 0.99,
        "Top chunk should have very high similarity: {}",
        similarities[0].1
    );

    // Bottom result should have lower similarity
    assert!(
        similarities[4].1 < 0.7,
        "Least similar chunk should have lower similarity: {}",
        similarities[4].1
    );
}

#[test]
fn test_dual_path_convergence() {
    // Both paths should return relevant results for the same query.
    // The symbolic path gives the EXACT source; the neural path gives
    // RELATED sources (which should include the exact source).

    let chunks = medical_chunks_batch();
    let embeddings = chunk_embeddings_batch();
    let mut store = ArrowGraphStore::new();

    // Triple linked to chunk_cpg_001
    let triple = Triple {
        subject: "nusy:Patient".to_string(),
        predicate: "nusy:recommended".to_string(),
        object: "nusy:Metformin".to_string(),
        graph: None,
        confidence: Some(0.95),
        source_document: Some("ada-guidelines.pdf".to_string()),
        source_chunk_id: Some("chunk_cpg_001".to_string()),
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

    // Symbolic path: get exact chunk
    let results = store
        .query(&QuerySpec {
            subject: Some("nusy:Patient".to_string()),
            ..Default::default()
        })
        .unwrap();
    let symbolic_chunk_id = results[0]
        .column(col::SOURCE_CHUNK_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
        .value(0)
        .to_string();

    // Neural path: find k-nearest chunks to the metformin concept
    let query_embedding: Vec<f32> = vec![0.88, 0.78, 0.12, 0.22];
    let entity_ids = embeddings
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let vectors = embeddings
        .column(1)
        .as_any()
        .downcast_ref::<FixedSizeListArray>()
        .unwrap();

    let mut neural_results: Vec<(String, f32)> = (0..embeddings.num_rows())
        .map(|i| {
            let id = entity_ids.value(i).to_string();
            let vec = vectors
                .value(i)
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap()
                .values()
                .to_vec();
            (id, cosine_similarity(&query_embedding, &vec))
        })
        .collect();
    neural_results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    // Take top-3 neural results
    let top_3: Vec<&str> = neural_results
        .iter()
        .take(3)
        .map(|(id, _)| id.as_str())
        .collect();

    // CONVERGENCE: The symbolic path's exact chunk should appear in the
    // neural path's top results
    assert!(
        top_3.contains(&symbolic_chunk_id.as_str()),
        "Symbolic chunk '{}' should appear in neural top-3: {:?}",
        symbolic_chunk_id,
        top_3
    );

    // Both paths should point to the same document
    let chunk_ids_col = chunks
        .column(chunk_col::CHUNK_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let doc_paths = chunks
        .column(chunk_col::DOCUMENT_PATH)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();

    let symbolic_row = (0..chunks.num_rows())
        .find(|&i| chunk_ids_col.value(i) == symbolic_chunk_id)
        .unwrap();
    let neural_top_row = (0..chunks.num_rows())
        .find(|&i| chunk_ids_col.value(i) == neural_results[0].0)
        .unwrap();

    assert_eq!(
        doc_paths.value(symbolic_row),
        doc_paths.value(neural_top_row),
        "Both paths should resolve to the same document"
    );
}

#[test]
fn test_null_source_chunk_id_fallback() {
    // When source_chunk_id is null, WHY chain falls back to source_document only
    let mut store = ArrowGraphStore::new();

    let triple = Triple {
        subject: "nusy:OldFact".to_string(),
        predicate: "rdf:type".to_string(),
        object: "nusy:Legacy".to_string(),
        graph: None,
        confidence: Some(0.8),
        source_document: Some("old-document.md".to_string()),
        source_chunk_id: None,
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
            subject: Some("nusy:OldFact".to_string()),
            ..Default::default()
        })
        .unwrap();
    let batch = &results[0];

    // source_chunk_id is null — no paragraph-level resolution possible
    let chunk_id = batch
        .column(col::SOURCE_CHUNK_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert!(chunk_id.is_null(0));

    // But source_document provides document-level WHY
    let doc = batch
        .column(col::SOURCE_DOCUMENT)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(doc.value(0), "old-document.md");
}
