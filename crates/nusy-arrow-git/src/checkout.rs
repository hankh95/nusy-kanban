//! Checkout — load a Parquet snapshot back into the ArrowGraphStore.
//!
//! Restores the graph state from a previous commit by reading the
//! namespace Parquet files and replacing the live store contents.

use crate::commit::{CommitError, CommitsTable};
use crate::object_store::GitObjectStore;
use nusy_arrow_core::Namespace;
use nusy_arrow_core::schema::normalize_to_current;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::fs;

pub type Result<T> = std::result::Result<T, CommitError>;

/// Checkout a previous commit: load its Parquet snapshots into the live store.
///
/// This replaces the current store contents with the committed state.
pub fn checkout(
    obj_store: &mut GitObjectStore,
    commits_table: &CommitsTable,
    commit_id: &str,
) -> Result<()> {
    // Verify the commit exists
    let _commit = commits_table
        .get(commit_id)
        .ok_or_else(|| CommitError::NotFound(commit_id.to_string()))?;

    // Clear the current store
    obj_store.store.clear();

    // Load each namespace's Parquet file if it exists
    for ns in Namespace::ALL {
        let path = obj_store.namespace_parquet_path(commit_id, ns.as_str());
        if !path.exists() {
            continue;
        }

        let file = fs::File::open(&path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;

        // Extract schema version from Parquet metadata (default to "1.0.0" if absent)
        let version = builder
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .and_then(|kv| {
                kv.iter()
                    .find(|e| e.key == "nusy_schema_version")
                    .and_then(|e| e.value.clone())
            })
            .unwrap_or_else(|| "1.0.0".to_string());

        let reader = builder.build()?;

        let mut batches = Vec::new();
        for batch_result in reader {
            let batch = batch_result?;
            // Normalize to current schema version on read
            let normalized = normalize_to_current(&batch, &version)?;
            batches.push(normalized);
        }

        obj_store.store.set_namespace_batches(ns, batches);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::create_commit;
    use nusy_arrow_core::{Namespace, QuerySpec, Triple, YLayer};

    fn sample_triple(subj: &str) -> Triple {
        Triple {
            subject: subj.to_string(),
            predicate: "rdf:type".to_string(),
            object: "Thing".to_string(),
            graph: None,
            confidence: Some(0.9),
            source_document: None,
            source_chunk_id: None,
            extracted_by: None,
            caused_by: None,
            derived_from: None,
            consolidated_at: None,
            certifiability_class: None,
            object_datatype: None,
        }
    }

    #[test]
    fn test_commit_checkout_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Add 1K triples and commit
        let triples: Vec<Triple> = (0..1000).map(|i| sample_triple(&format!("s{i}"))).collect();
        obj.store
            .add_batch(&triples, Namespace::World, YLayer::Semantic)
            .unwrap();

        let c1 = create_commit(&obj, &mut commits, vec![], "with 1K", "DGX").unwrap();
        assert_eq!(obj.store.len(), 1000);

        // Add 500 more
        let more: Vec<Triple> = (1000..1500)
            .map(|i| sample_triple(&format!("s{i}")))
            .collect();
        obj.store
            .add_batch(&more, Namespace::World, YLayer::Semantic)
            .unwrap();
        assert_eq!(obj.store.len(), 1500);

        // Checkout previous commit — should have only 1K
        checkout(&mut obj, &commits, &c1.commit_id).unwrap();
        assert_eq!(obj.store.len(), 1000);

        // Verify subjects: s0-s999 should exist, s1000+ should not
        let q = obj
            .store
            .query(&QuerySpec {
                subject: Some("s0".to_string()),
                ..Default::default()
            })
            .unwrap();
        let count: usize = q.iter().map(|b| b.num_rows()).sum();
        assert_eq!(count, 1, "s0 should exist after checkout");

        let q2 = obj
            .store
            .query(&QuerySpec {
                subject: Some("s1000".to_string()),
                ..Default::default()
            })
            .unwrap();
        let count2: usize = q2.iter().map(|b| b.num_rows()).sum();
        assert_eq!(count2, 0, "s1000 should NOT exist after checkout");
    }

    #[test]
    fn test_checkout_nonexistent_commit_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let commits = CommitsTable::new();

        let result = checkout(&mut obj, &commits, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_commit_checkout_multiple_namespaces() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Add to world and work
        obj.store
            .add_triple(
                &sample_triple("world-s"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        obj.store
            .add_triple(
                &sample_triple("work-s"),
                Namespace::Work,
                YLayer::Procedural,
            )
            .unwrap();

        let c1 = create_commit(&obj, &mut commits, vec![], "multi-ns", "DGX").unwrap();

        // Clear and checkout
        obj.store.clear();
        assert_eq!(obj.store.len(), 0);

        checkout(&mut obj, &commits, &c1.commit_id).unwrap();
        assert_eq!(obj.store.len(), 2);
    }

    #[test]
    fn test_commit_checkout_benchmark_10k() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        let triples: Vec<Triple> = (0..10_000)
            .map(|i| sample_triple(&format!("bench{i}")))
            .collect();
        obj.store
            .add_batch(&triples, Namespace::World, YLayer::Semantic)
            .unwrap();

        // Benchmark commit
        let start = std::time::Instant::now();
        let c1 = create_commit(&obj, &mut commits, vec![], "bench", "DGX").unwrap();
        let commit_ms = start.elapsed().as_millis();

        // Benchmark checkout
        obj.store.clear();
        let start = std::time::Instant::now();
        checkout(&mut obj, &commits, &c1.commit_id).unwrap();
        let checkout_ms = start.elapsed().as_millis();

        assert_eq!(obj.store.len(), 10_000);

        let total = commit_ms + checkout_ms;
        eprintln!("10K commit: {commit_ms}ms, checkout: {checkout_ms}ms, total: {total}ms");
        // Target: <50ms for commit+checkout round-trip
        // Allow generous margin for CI — the important thing is it's fast
        assert!(
            total < 500,
            "Round-trip took {total}ms — should be well under 500ms"
        );
    }
}
