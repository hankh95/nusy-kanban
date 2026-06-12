//! Diff — object-level comparison between two commits.
//!
//! Compares RecordBatches to find added, removed, and modified triples.
//! A triple is identified by (subject, predicate, object, namespace).

use crate::checkout;
use crate::commit::{CommitError, CommitsTable};
use crate::object_store::GitObjectStore;
use arrow::array::{Array, Float64Array, StringArray, TimestampMillisecondArray, UInt8Array};
use nusy_arrow_core::{QuerySpec, col};
use std::collections::HashMap;

/// A single diff entry, carrying full provenance metadata so merges preserve it.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffEntry {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub namespace: String,
    pub y_layer: u8,
    pub confidence: Option<f64>,
    pub graph: Option<String>,
    pub source_document: Option<String>,
    pub source_chunk_id: Option<String>,
    pub caused_by: Option<String>,
    pub derived_from: Option<String>,
    pub consolidated_at: Option<i64>,
    pub certifiability_class: Option<String>,
}

/// The result of a diff between two commits.
#[derive(Debug, Clone, Default)]
pub struct DiffResult {
    /// Triples present in `head` but not in `base`.
    pub added: Vec<DiffEntry>,
    /// Triples present in `base` but not in `head`.
    pub removed: Vec<DiffEntry>,
}

impl DiffResult {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    pub fn total_changes(&self) -> usize {
        self.added.len() + self.removed.len()
    }
}

/// A triple key for set comparison (identity = subject + predicate + object + namespace).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TripleKey {
    subject: String,
    predicate: String,
    object: String,
    namespace: String,
}

/// Extract all triples (key → full DiffEntry) from the store's current state.
fn extract_triples(store: &nusy_arrow_core::ArrowGraphStore) -> HashMap<TripleKey, DiffEntry> {
    let mut map = HashMap::new();

    let batches = store
        .query(&QuerySpec {
            include_deleted: false,
            ..Default::default()
        })
        .unwrap_or_default();

    for batch in &batches {
        let subjects = batch
            .column(col::SUBJECT)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("subject column");
        let predicates = batch
            .column(col::PREDICATE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("predicate column");
        let objects = batch
            .column(col::OBJECT)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("object column");
        let graphs = batch
            .column(col::GRAPH)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("graph column");
        let namespaces = batch
            .column(col::NAMESPACE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("namespace column");
        let y_layers = batch
            .column(col::Y_LAYER)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .expect("y_layer column");
        let confidences = batch
            .column(col::CONFIDENCE)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("confidence column");
        let source_docs = batch
            .column(col::SOURCE_DOCUMENT)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("source_document column");
        let source_chunks = batch
            .column(col::SOURCE_CHUNK_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("source_chunk_id column");
        let caused_bys = batch
            .column(col::CAUSED_BY)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("caused_by column");
        let derived_froms = batch
            .column(col::DERIVED_FROM)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("derived_from column");
        let consolidated_ats = batch
            .column(col::CONSOLIDATED_AT)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .expect("consolidated_at column");
        let certifiability_classes = batch
            .column(col::CERTIFIABILITY_CLASS)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("certifiability_class column");

        for i in 0..batch.num_rows() {
            let key = TripleKey {
                subject: subjects.value(i).to_string(),
                predicate: predicates.value(i).to_string(),
                object: objects.value(i).to_string(),
                namespace: namespaces.value(i).to_string(),
            };
            let entry = DiffEntry {
                subject: key.subject.clone(),
                predicate: key.predicate.clone(),
                object: key.object.clone(),
                namespace: key.namespace.clone(),
                y_layer: y_layers.value(i),
                confidence: if confidences.is_null(i) {
                    None
                } else {
                    Some(confidences.value(i))
                },
                graph: if graphs.is_null(i) {
                    None
                } else {
                    Some(graphs.value(i).to_string())
                },
                source_document: if source_docs.is_null(i) {
                    None
                } else {
                    Some(source_docs.value(i).to_string())
                },
                source_chunk_id: if source_chunks.is_null(i) {
                    None
                } else {
                    Some(source_chunks.value(i).to_string())
                },
                caused_by: if caused_bys.is_null(i) {
                    None
                } else {
                    Some(caused_bys.value(i).to_string())
                },
                derived_from: if derived_froms.is_null(i) {
                    None
                } else {
                    Some(derived_froms.value(i).to_string())
                },
                consolidated_at: if consolidated_ats.is_null(i) {
                    None
                } else {
                    Some(consolidated_ats.value(i))
                },
                certifiability_class: if certifiability_classes.is_null(i) {
                    None
                } else {
                    Some(certifiability_classes.value(i).to_string())
                },
            };
            map.insert(key, entry);
        }
    }

    map
}

/// Compute the diff between two commits.
///
/// `base` is the earlier commit, `head` is the later commit.
/// Returns triples added in head and triples removed from base.
///
/// # Safety
///
/// **This function replaces the live store contents** by calling `checkout()` internally.
/// Any uncommitted changes in `obj_store` will be lost. The store will contain the
/// `head` commit's state when this function returns. Callers should commit or save
/// any in-progress work before calling `diff()`.
pub fn diff(
    obj_store: &mut GitObjectStore,
    commits_table: &CommitsTable,
    base_commit_id: &str,
    head_commit_id: &str,
) -> Result<DiffResult, CommitError> {
    // Load base state
    checkout::checkout(obj_store, commits_table, base_commit_id)?;
    let base_triples = extract_triples(&obj_store.store);

    // Load head state
    checkout::checkout(obj_store, commits_table, head_commit_id)?;
    let head_triples = extract_triples(&obj_store.store);

    // Added = in head but not in base (with full metadata from head)
    let added: Vec<DiffEntry> = head_triples
        .iter()
        .filter(|(k, _)| !base_triples.contains_key(k))
        .map(|(_, entry)| entry.clone())
        .collect();

    // Removed = in base but not in head (with full metadata from base)
    let removed: Vec<DiffEntry> = base_triples
        .iter()
        .filter(|(k, _)| !head_triples.contains_key(k))
        .map(|(_, entry)| entry.clone())
        .collect();

    Ok(DiffResult { added, removed })
}

/// Compute diff without mutating the store — saves and restores current state.
///
/// Use this when you have uncommitted changes you want to preserve.
pub fn diff_nondestructive(
    obj_store: &mut GitObjectStore,
    commits_table: &CommitsTable,
    base_commit_id: &str,
    head_commit_id: &str,
) -> Result<DiffResult, CommitError> {
    // Save current state
    let saved: Vec<(nusy_arrow_core::Namespace, Vec<arrow::array::RecordBatch>)> =
        nusy_arrow_core::Namespace::ALL
            .iter()
            .map(|ns| {
                let batches = obj_store.store.get_namespace_batches(*ns).to_vec();
                (*ns, batches)
            })
            .collect();

    let result = diff(obj_store, commits_table, base_commit_id, head_commit_id);

    // Restore previous state
    for (ns, batches) in saved {
        obj_store.store.set_namespace_batches(ns, batches);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::create_commit;
    use nusy_arrow_core::{Namespace, Triple, YLayer};

    fn sample_triple(subj: &str, obj: &str) -> Triple {
        Triple {
            subject: subj.to_string(),
            predicate: "rdf:type".to_string(),
            object: obj.to_string(),
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
    fn test_diff_detects_additions() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Commit with 1 triple
        obj.store
            .add_triple(
                &sample_triple("s1", "A"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c1 = create_commit(&obj, &mut commits, vec![], "first", "DGX").unwrap();

        // Add another triple and commit
        obj.store
            .add_triple(
                &sample_triple("s2", "B"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c2 = create_commit(
            &obj,
            &mut commits,
            vec![c1.commit_id.clone()],
            "second",
            "DGX",
        )
        .unwrap();

        let result = diff(&mut obj, &commits, &c1.commit_id, &c2.commit_id).unwrap();
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.removed.len(), 0);
        assert_eq!(result.added[0].subject, "s2");
        // Verify metadata is preserved
        assert_eq!(result.added[0].y_layer, YLayer::Semantic.as_u8());
        assert_eq!(result.added[0].confidence, Some(0.9));
    }

    #[test]
    fn test_diff_detects_removals() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Commit with 2 triples
        obj.store
            .add_triple(
                &sample_triple("s1", "A"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let id2 = obj
            .store
            .add_triple(
                &sample_triple("s2", "B"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c1 = create_commit(&obj, &mut commits, vec![], "first", "DGX").unwrap();

        // Delete one and commit
        obj.store.delete(&id2).unwrap();
        let c2 = create_commit(
            &obj,
            &mut commits,
            vec![c1.commit_id.clone()],
            "second",
            "DGX",
        )
        .unwrap();

        let result = diff(&mut obj, &commits, &c1.commit_id, &c2.commit_id).unwrap();
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].subject, "s2");
    }

    #[test]
    fn test_diff_nondestructive_preserves_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        obj.store
            .add_triple(
                &sample_triple("s1", "A"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c1 = create_commit(&obj, &mut commits, vec![], "first", "DGX").unwrap();

        obj.store
            .add_triple(
                &sample_triple("s2", "B"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c2 = create_commit(
            &obj,
            &mut commits,
            vec![c1.commit_id.clone()],
            "second",
            "DGX",
        )
        .unwrap();

        // Add uncommitted work
        obj.store
            .add_triple(
                &sample_triple("uncommitted", "X"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        assert_eq!(obj.store.len(), 3); // s1 + s2 + uncommitted

        // Nondestructive diff should preserve uncommitted state
        let result = diff_nondestructive(&mut obj, &commits, &c1.commit_id, &c2.commit_id).unwrap();
        assert_eq!(result.added.len(), 1);

        // Uncommitted work should still be there
        assert_eq!(obj.store.len(), 3);
    }

    #[test]
    fn test_diff_no_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        obj.store
            .add_triple(
                &sample_triple("s1", "A"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c1 = create_commit(&obj, &mut commits, vec![], "first", "DGX").unwrap();

        // Commit same state again
        let c2 = create_commit(
            &obj,
            &mut commits,
            vec![c1.commit_id.clone()],
            "same",
            "DGX",
        )
        .unwrap();

        let result = diff(&mut obj, &commits, &c1.commit_id, &c2.commit_id).unwrap();
        assert!(result.is_empty());
    }
}
