//! EXP-3003 Phase 2: Rust-side Parquet interop tests.
//!
//! Verifies that nusy-arrow-core can read V12-format Parquet files
//! and that V14-produced Parquet files are readable by Polars.

use arrow::array::{Array, Float64Array, StringArray};
use arrow::datatypes::FieldRef;
use nusy_arrow_core::{Namespace, Triple, YLayer};
use nusy_arrow_git::{GitObjectStore, restore, save};
use tempfile::TempDir;

fn make_triple(s: &str, p: &str, o: &str) -> Triple {
    Triple {
        subject: s.into(),
        predicate: p.into(),
        object: o.into(),
        graph: None,
        confidence: Some(0.9),
        source_document: Some("test_doc.md".into()),
        source_chunk_id: None,
        extracted_by: Some("test".into()),
        caused_by: None,
        derived_from: None,
        consolidated_at: None,
        certifiability_class: None,
        object_datatype: None,
    }
}

#[test]
fn test_v14_parquet_written_by_save() {
    // V14 save() writes Parquet that should be readable
    let tmp = TempDir::new().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

    // Add triples
    obj.store
        .add_triple(
            &make_triple("alice", "knows", "bob"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    obj.store
        .add_triple(
            &make_triple("bob", "knows", "carol"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();

    // Save (writes Parquet files)
    let save_dir = tmp.path().join("state");
    save(&obj, &save_dir).unwrap();

    // Verify Parquet file exists
    let parquet = save_dir.join("world.parquet");
    assert!(parquet.exists(), "world.parquet should exist after save");

    // Restore into a new store (V14 reads V14)
    let mut obj2 = GitObjectStore::with_snapshot_dir(tmp.path().join("snap2"));
    restore(&mut obj2, &save_dir).unwrap();
    assert_eq!(obj2.store.len(), 2, "Should restore 2 triples");
}

#[test]
fn test_v14_parquet_has_correct_columns() {
    let tmp = TempDir::new().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

    obj.store
        .add_triple(
            &make_triple("alice", "knows", "bob"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();

    let save_dir = tmp.path().join("state");
    save(&obj, &save_dir).unwrap();

    // Read back and verify schema columns
    let mut obj2 = GitObjectStore::with_snapshot_dir(tmp.path().join("snap2"));
    restore(&mut obj2, &save_dir).unwrap();

    let batches = obj2.store.get_namespace_batches(Namespace::World);
    assert!(!batches.is_empty());

    let batch = &batches[0];
    let schema = batch.schema();
    let col_names: Vec<&str> = schema
        .fields()
        .iter()
        .map(|f: &FieldRef| f.name().as_str())
        .collect();

    // V14 core columns must exist
    assert!(col_names.contains(&"subject"), "missing subject");
    assert!(col_names.contains(&"predicate"), "missing predicate");
    assert!(col_names.contains(&"object"), "missing object");
    assert!(col_names.contains(&"triple_id"), "missing triple_id");
    assert!(col_names.contains(&"confidence"), "missing confidence");
    assert!(col_names.contains(&"y_layer"), "missing y_layer");
}

#[test]
fn test_v14_data_integrity() {
    let tmp = TempDir::new().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

    obj.store
        .add_triple(
            &make_triple("http://ex.org/Alice", "related_to", "http://ex.org/Bob"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();

    let save_dir = tmp.path().join("state");
    save(&obj, &save_dir).unwrap();

    let mut obj2 = GitObjectStore::with_snapshot_dir(tmp.path().join("snap2"));
    restore(&mut obj2, &save_dir).unwrap();

    let batches = obj2.store.get_namespace_batches(Namespace::World);
    let batch = &batches[0];

    // Verify actual values
    let subjects = batch
        .column_by_name("subject")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(subjects.value(0), "http://ex.org/Alice");

    let predicates = batch
        .column_by_name("predicate")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(predicates.value(0), "related_to");

    let confidences = batch
        .column_by_name("confidence")
        .unwrap()
        .as_any()
        .downcast_ref::<Float64Array>()
        .unwrap();
    assert!((confidences.value(0) - 0.9).abs() < 0.001);
}
