//! Integration tests for nusy-tutor-record:
//! - Round-trip serde (JSON → Rust → JSON, value-equivalent)
//! - JSON Schema validation against the embedded schema
//! - Cross-field semantic validation against the canonical example
//! - File I/O sanity check (on-disk bytes parse cleanly)

use nusy_tutor_record::{
    CqKind, Level, SCHEMA_JSON, Salience, TutorRecord, ValidationError, validate_against_schema,
};

const DAME_WONDER_JSON: &str = include_str!("../examples/dame_wonder.json");

#[test]
fn dame_wonder_example_round_trips() {
    // 1. JSON → Rust (deserialize)
    let record: TutorRecord = serde_json::from_str(DAME_WONDER_JSON)
        .expect("canonical Dame Wonder example must deserialize");

    // 2. Rust → JSON (serialize)
    let serialized = serde_json::to_value(&record).expect("must serialize");

    // 3. JSON → Rust (re-deserialize from the round-tripped value)
    let reparsed: TutorRecord =
        serde_json::from_value(serialized.clone()).expect("must re-deserialize");

    // 4. Idempotence: original Rust value equals re-parsed Rust value
    assert_eq!(
        record, reparsed,
        "round-trip must be idempotent at the Rust value level"
    );
}

#[test]
fn dame_wonder_passes_semantic_validation() {
    let record: TutorRecord = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    record
        .validate_semantics()
        .expect("canonical example must pass semantic validation");
}

#[test]
fn dame_wonder_passes_json_schema() {
    let value: serde_json::Value = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    validate_against_schema(&value).expect("canonical example must pass JSON Schema validation");
}

#[test]
fn dame_wonder_layer_counts_match_expectations() {
    let record: TutorRecord = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    assert_eq!(record.layers.layer_1_literal.chunks.len(), 4);
    assert_eq!(record.layers.layer_1_literal.triples.len(), 9);
    assert_eq!(record.layers.layer_2_ontology.entities.len(), 3);
    assert_eq!(record.layers.layer_3_curiosity.cqs.len(), 6);
    assert_eq!(record.layers.layer_4_cross_book.anchors.len(), 3);
    assert_eq!(record.layers.layer_5_multimodal.illustrations.len(), 2);
}

#[test]
fn dame_wonder_uses_all_cq_kinds_we_advertise() {
    // The canonical example should exercise the variety of CqKind variants so
    // that schema consumers see real usage. Every kind that appears in this
    // example must round-trip through serde correctly.
    let record: TutorRecord = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    let kinds: std::collections::HashSet<_> = record
        .layers
        .layer_3_curiosity
        .cqs
        .iter()
        .map(|c| c.kind)
        .collect();

    assert!(kinds.contains(&CqKind::DirectWordMeaning));
    assert!(kinds.contains(&CqKind::CausalChain));
    assert!(kinds.contains(&CqKind::PatternRecognition));
    assert!(kinds.contains(&CqKind::Metacognitive));
    assert!(kinds.contains(&CqKind::Multimodal));
}

#[test]
fn dame_wonder_metadata_matches_seed() {
    let record: TutorRecord = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    assert_eq!(record.document.level, Level::L0_toddler);
    assert_eq!(record.document.era.as_deref(), Some("Victorian"));
    assert_eq!(
        record.document.publisher.as_deref(),
        Some("McLoughlin Brothers")
    );
}

#[test]
fn skip_role_chunks_carry_skip_salience() {
    // Sanity: license boilerplate in the canonical example should be marked
    // salience=skip so consumers don't deep-process it.
    let record: TutorRecord = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    let license_chunk = record
        .layers
        .layer_1_literal
        .chunks
        .iter()
        .find(|c| c.role.as_deref() == Some("license_boilerplate"))
        .expect("Dame Wonder example must include a license_boilerplate chunk");
    assert_eq!(license_chunk.salience, Some(Salience::Skip));
}

#[test]
fn schema_file_compiles_as_json_schema_2020_12() {
    let schema_value: serde_json::Value =
        serde_json::from_str(SCHEMA_JSON).expect("schema is valid JSON");
    jsonschema::draft202012::new(&schema_value)
        .expect("schema is a valid JSON Schema 2020-12 document");
}

#[test]
fn schema_rejects_missing_required_field() {
    // Removing a required field at the document level must trip JSON Schema.
    let mut value: serde_json::Value = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    value.as_object_mut().unwrap().remove("layers");
    match validate_against_schema(&value) {
        Err(ValidationError::JsonSchema { .. }) => {}
        other => panic!("expected JsonSchema rejection, got {other:?}"),
    }
}

#[test]
fn schema_rejects_invalid_level_enum() {
    let mut value: serde_json::Value = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    value["document"]["level"] = serde_json::json!("L99_phd");
    match validate_against_schema(&value) {
        Err(ValidationError::JsonSchema { .. }) => {}
        other => panic!("expected JsonSchema rejection for bad level enum, got {other:?}"),
    }
}

#[test]
fn schema_rejects_malformed_chunk_id_pattern() {
    // chunk_id pattern requires `chunk_NNN`; a stray suffix should fail.
    let mut value: serde_json::Value = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    value["layers"]["layer_1_literal"]["chunks"][0]["id"] = serde_json::json!("not-a-chunk");
    match validate_against_schema(&value) {
        Err(ValidationError::JsonSchema { .. }) => {}
        other => panic!("expected JsonSchema pattern rejection, got {other:?}"),
    }
}

#[test]
fn schema_rejects_scoring_out_of_range() {
    let mut value: serde_json::Value = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    value["scoring"]["recall_target"] = serde_json::json!(2.0);
    match validate_against_schema(&value) {
        Err(ValidationError::JsonSchema { .. }) => {}
        other => panic!("expected JsonSchema range rejection, got {other:?}"),
    }
}

#[test]
fn deserialize_rejects_unknown_field() {
    // serde with deny_unknown_fields catches typos / forward-compat drift.
    let mut value: serde_json::Value = serde_json::from_str(DAME_WONDER_JSON).unwrap();
    value["document"]["typo_field"] = serde_json::json!("oops");
    let result: Result<TutorRecord, _> = serde_json::from_value(value);
    assert!(
        result.is_err(),
        "serde must reject unknown fields in DocumentRef"
    );
}

#[test]
fn file_on_disk_is_canonical() {
    // Read the example from disk and check it parses identically to the
    // include_str! version. Catches editor mojibake / encoding drift.
    let from_disk = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/dame_wonder.json"
    ))
    .expect("examples/dame_wonder.json must be readable");
    assert_eq!(
        from_disk.trim_end(),
        DAME_WONDER_JSON.trim_end(),
        "embedded and on-disk Dame Wonder must match byte-for-byte"
    );
}
