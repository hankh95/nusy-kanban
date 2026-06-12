//! Cherry-pick — apply a specific commit's changes to the current branch.
//!
//! Computes the diff introduced by the source commit, then applies those
//! changes to HEAD, creating a new commit with the current HEAD as parent.

use crate::checkout;
use crate::commit::{CommitError, CommitsTable, create_commit};
use crate::diff;
use crate::object_store::GitObjectStore;
use nusy_arrow_core::{Namespace, Triple, YLayer, col};
use std::collections::HashSet;

/// Errors from cherry-pick operations.
#[derive(Debug, thiserror::Error)]
pub enum CherryPickError {
    #[error("Commit error: {0}")]
    Commit(#[from] CommitError),

    #[error("Store error: {0}")]
    Store(#[from] nusy_arrow_core::StoreError),

    #[error("Commit has no parent: {0}")]
    NoParent(String),

    #[error("Cherry-pick conflict: {0} triples conflict with HEAD")]
    Conflict(usize),
}

/// Cherry-pick a commit onto the current HEAD.
///
/// 1. Compute what the source commit changed (diff parent → source)
/// 2. Check if any of those changes conflict with HEAD
/// 3. If clean, apply the diff and create a new commit
///
/// Returns the new commit's ID.
pub fn cherry_pick(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    source_commit_id: &str,
    head_commit_id: &str,
    author: &str,
) -> Result<String, CherryPickError> {
    let source = commits_table
        .get(source_commit_id)
        .ok_or_else(|| CommitError::NotFound(source_commit_id.to_string()))?;

    if source.parent_ids.is_empty() {
        return Err(CherryPickError::NoParent(source_commit_id.to_string()));
    }

    let parent_id = source.parent_ids[0].clone();
    let source_message = source.message.clone();

    // Compute what the source commit changed
    let source_diff = diff::diff(obj_store, commits_table, &parent_id, source_commit_id)?;

    // Restore HEAD state
    checkout::checkout(obj_store, commits_table, head_commit_id)?;

    // Check for conflicts: if HEAD already has a triple with the same
    // (subject, predicate, namespace) but different object, that's a conflict
    let mut conflict_count = 0;
    for entry in &source_diff.added {
        // Defensive: fall back to World namespace if the diff entry has an
        // unrecognized namespace string. This can happen if namespaces were
        // added after the commit was created.
        let ns = Namespace::from_str_loose(&entry.namespace).unwrap_or(Namespace::World);
        let batches = obj_store.store.get_namespace_batches(ns);
        for batch in batches {
            let subj_col = batch
                .column(col::SUBJECT)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("subject column");
            let pred_col = batch
                .column(col::PREDICATE)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("predicate column");
            let obj_col = batch
                .column(col::OBJECT)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("object column");

            for i in 0..batch.num_rows() {
                if subj_col.value(i) == entry.subject
                    && pred_col.value(i) == entry.predicate
                    && obj_col.value(i) != entry.object
                {
                    conflict_count += 1;
                }
            }
        }
    }

    if conflict_count > 0 {
        return Err(CherryPickError::Conflict(conflict_count));
    }

    // Apply additions
    for entry in &source_diff.added {
        // Defensive fallbacks: if namespace or y_layer values from the diff
        // are unrecognized (e.g., schema evolved), default to World/Semantic
        // rather than failing the entire cherry-pick.
        let ns = Namespace::from_str_loose(&entry.namespace).unwrap_or(Namespace::World);
        let y_layer = YLayer::from_u8(entry.y_layer).unwrap_or(YLayer::Semantic);

        // Skip if HEAD already has this exact triple
        let already_exists = {
            let batches = obj_store.store.get_namespace_batches(ns);
            let mut found = false;
            for batch in batches {
                let subj_col = batch
                    .column(col::SUBJECT)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .expect("subject column");
                let pred_col = batch
                    .column(col::PREDICATE)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .expect("predicate column");
                let obj_col = batch
                    .column(col::OBJECT)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .expect("object column");

                for i in 0..batch.num_rows() {
                    if subj_col.value(i) == entry.subject
                        && pred_col.value(i) == entry.predicate
                        && obj_col.value(i) == entry.object
                    {
                        found = true;
                        break;
                    }
                }
                if found {
                    break;
                }
            }
            found
        };

        if !already_exists {
            let triple = Triple {
                subject: entry.subject.clone(),
                predicate: entry.predicate.clone(),
                object: entry.object.clone(),
                graph: entry.graph.clone(),
                confidence: entry.confidence,
                source_document: entry.source_document.clone(),
                source_chunk_id: entry.source_chunk_id.clone(),
                extracted_by: Some(format!("cherry-pick by {author}")),
                caused_by: entry.caused_by.clone(),
                derived_from: entry.derived_from.clone(),
                consolidated_at: entry.consolidated_at,
                certifiability_class: entry.certifiability_class.clone(),
                object_datatype: None,
            };
            obj_store.store.add_triple(&triple, ns, y_layer)?;
        }
    }

    // Apply removals — collect IDs to delete first to avoid borrow issues
    let removals: HashSet<(String, String, String, String)> = source_diff
        .removed
        .iter()
        .map(|e| {
            (
                e.subject.clone(),
                e.predicate.clone(),
                e.object.clone(),
                e.namespace.clone(),
            )
        })
        .collect();

    for ns in Namespace::ALL {
        let ns_str = ns.as_str().to_string();
        let batches = obj_store.store.get_namespace_batches(ns);
        let mut ids_to_delete = Vec::new();
        for batch in batches {
            let id_col = batch
                .column(col::TRIPLE_ID)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("triple_id column");
            let subj_col = batch
                .column(col::SUBJECT)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("subject column");
            let pred_col = batch
                .column(col::PREDICATE)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("predicate column");
            let obj_col = batch
                .column(col::OBJECT)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("object column");

            for i in 0..batch.num_rows() {
                let key = (
                    subj_col.value(i).to_string(),
                    pred_col.value(i).to_string(),
                    obj_col.value(i).to_string(),
                    ns_str.clone(),
                );
                if removals.contains(&key) {
                    ids_to_delete.push(id_col.value(i).to_string());
                }
            }
        }
        for id in &ids_to_delete {
            // Best-effort delete: triple may already have been removed by a
            // prior operation or may not exist in this snapshot. Swallowing
            // the error is intentional — the removal set comes from a diff
            // against a different commit, so missing IDs are expected.
            let _ = obj_store.store.delete(id);
        }
    }

    // Create cherry-pick commit with HEAD as parent
    let cp_commit = create_commit(
        obj_store,
        commits_table,
        vec![head_commit_id.to_string()],
        &format!("Cherry-pick: {source_message}"),
        author,
    )?;

    Ok(cp_commit.commit_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkout::checkout as git_checkout;
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
    fn test_clean_cherry_pick() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Base: s1
        obj.store
            .add_triple(
                &sample_triple("s1", "Base"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

        // Feature branch: add s2
        obj.store
            .add_triple(
                &sample_triple("s2", "Feature"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let feature = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "add s2",
            "DGX",
        )
        .unwrap();

        // Main branch: add s3 (diverge from base)
        git_checkout(&mut obj, &commits, &base.commit_id).unwrap();
        obj.store
            .add_triple(
                &sample_triple("s3", "Main"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let main_head = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "add s3",
            "DGX",
        )
        .unwrap();

        // Cherry-pick the feature commit onto main
        let cp_id = cherry_pick(
            &mut obj,
            &mut commits,
            &feature.commit_id,
            &main_head.commit_id,
            "DGX",
        )
        .unwrap();

        // Main should now have s1 + s3 + s2 (cherry-picked)
        assert_eq!(obj.store.len(), 3);

        // Verify the cherry-pick commit
        let cp = commits.get(&cp_id).unwrap();
        assert!(cp.message.starts_with("Cherry-pick:"));
        assert_eq!(cp.parent_ids, vec![main_head.commit_id]);
    }

    #[test]
    fn test_cherry_pick_with_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Base
        obj.store
            .add_triple(
                &sample_triple("s1", "Base"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

        // Feature: add (conflict-subj, rdf:type, TypeA)
        obj.store
            .add_triple(
                &sample_triple("conflict-subj", "TypeA"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let feature = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "feature",
            "DGX",
        )
        .unwrap();

        // Main: add (conflict-subj, rdf:type, TypeB) — conflicts!
        git_checkout(&mut obj, &commits, &base.commit_id).unwrap();
        obj.store
            .add_triple(
                &sample_triple("conflict-subj", "TypeB"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let main_head = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "main",
            "DGX",
        )
        .unwrap();

        // Cherry-pick should detect the conflict
        let result = cherry_pick(
            &mut obj,
            &mut commits,
            &feature.commit_id,
            &main_head.commit_id,
            "DGX",
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            CherryPickError::Conflict(n) => assert!(n > 0),
            other => panic!("Expected Conflict, got: {other:?}"),
        }
    }

    #[test]
    fn test_cherry_pick_preserves_content() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Base
        obj.store
            .add_triple(
                &sample_triple("s1", "Base"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

        // Feature: add multiple triples
        obj.store
            .add_triple(
                &sample_triple("feat1", "F1"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        obj.store
            .add_triple(
                &sample_triple("feat2", "F2"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let feature = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "feature",
            "DGX",
        )
        .unwrap();

        // Go back to base for cherry-pick
        git_checkout(&mut obj, &commits, &base.commit_id).unwrap();
        let main_head = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "main",
            "DGX",
        )
        .unwrap();

        cherry_pick(
            &mut obj,
            &mut commits,
            &feature.commit_id,
            &main_head.commit_id,
            "DGX",
        )
        .unwrap();

        // Should have s1 + feat1 + feat2
        assert_eq!(obj.store.len(), 3);
    }
}
