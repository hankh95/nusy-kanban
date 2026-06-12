//! Revert — create a new commit that undoes the changes from a target commit.
//!
//! Unlike `checkout`, revert does not go back in time — it creates a NEW commit
//! on the current branch that applies the inverse of the target commit's diff.

use crate::checkout;
use crate::commit::{CommitError, CommitsTable, create_commit};
use crate::diff;
use crate::object_store::GitObjectStore;
use nusy_arrow_core::{Namespace, Triple, YLayer, col};

/// Errors from revert operations.
#[derive(Debug, thiserror::Error)]
pub enum RevertError {
    #[error("Commit error: {0}")]
    Commit(#[from] CommitError),

    #[error("Store error: {0}")]
    Store(#[from] nusy_arrow_core::StoreError),

    #[error("Cannot revert merge commit {0} (has {1} parents) — specify parent")]
    MergeCommit(String, usize),

    #[error("Commit has no parent: {0}")]
    NoParent(String),
}

/// Revert a commit by creating a new commit that undoes its changes.
///
/// 1. Find the target commit's parent
/// 2. Diff parent → target to get what the commit changed
/// 3. Apply the inverse (add removed triples, remove added triples) to HEAD
/// 4. Create a new commit with the inverted changes
///
/// Returns the new revert commit's ID.
pub fn revert(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    commit_id: &str,
    head_commit_id: &str,
    author: &str,
) -> Result<String, RevertError> {
    let target = commits_table
        .get(commit_id)
        .ok_or_else(|| CommitError::NotFound(commit_id.to_string()))?;

    // Cannot revert merge commits (multiple parents)
    if target.parent_ids.len() > 1 {
        return Err(RevertError::MergeCommit(
            commit_id.to_string(),
            target.parent_ids.len(),
        ));
    }

    // Must have a parent to compute the diff
    if target.parent_ids.is_empty() {
        return Err(RevertError::NoParent(commit_id.to_string()));
    }

    let parent_id = target.parent_ids[0].clone();
    let target_message = target.message.clone();

    // Compute what the target commit changed: diff parent → target
    let commit_diff = diff::diff(obj_store, commits_table, &parent_id, commit_id)?;

    // Restore HEAD state
    checkout::checkout(obj_store, commits_table, head_commit_id)?;

    // Apply the INVERSE:
    // - What was added by the commit should be removed
    // - What was removed by the commit should be re-added

    // Remove the added triples
    for entry in &commit_diff.added {
        // Defensive: fall back to World namespace if the diff entry has an
        // unrecognized namespace string (schema evolution).
        let ns = Namespace::from_str_loose(&entry.namespace).unwrap_or(Namespace::World);
        // Find and delete matching triples in the store
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
                if subj_col.value(i) == entry.subject
                    && pred_col.value(i) == entry.predicate
                    && obj_col.value(i) == entry.object
                {
                    ids_to_delete.push(id_col.value(i).to_string());
                }
            }
        }
        for id in &ids_to_delete {
            // Best-effort delete: triple may not exist in HEAD if state
            // diverged since the original commit. Swallowing the error is
            // intentional — the diff was computed against a different commit.
            let _ = obj_store.store.delete(id);
        }
    }

    // Re-add the removed triples
    for entry in &commit_diff.removed {
        // Defensive fallbacks: if namespace or y_layer values from the diff
        // are unrecognized (e.g., schema evolved), default to World/Semantic
        // rather than failing the entire revert.
        let ns = Namespace::from_str_loose(&entry.namespace).unwrap_or(Namespace::World);
        let y_layer = YLayer::from_u8(entry.y_layer).unwrap_or(YLayer::Semantic);
        let triple = Triple {
            subject: entry.subject.clone(),
            predicate: entry.predicate.clone(),
            object: entry.object.clone(),
            graph: entry.graph.clone(),
            confidence: entry.confidence,
            source_document: entry.source_document.clone(),
            source_chunk_id: entry.source_chunk_id.clone(),
            extracted_by: Some(format!("revert by {author}")),
            caused_by: entry.caused_by.clone(),
            derived_from: entry.derived_from.clone(),
            consolidated_at: entry.consolidated_at,
            certifiability_class: entry.certifiability_class.clone(),
            object_datatype: None,
        };
        obj_store.store.add_triple(&triple, ns, y_layer)?;
    }

    // Create the revert commit
    let revert_commit = create_commit(
        obj_store,
        commits_table,
        vec![head_commit_id.to_string()],
        &format!("Revert: {target_message}"),
        author,
    )?;

    Ok(revert_commit.commit_id)
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
    fn test_revert_restores_previous_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Commit A: one triple
        obj.store
            .add_triple(
                &sample_triple("s1", "A"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let ca = create_commit(&obj, &mut commits, vec![], "commit A", "DGX").unwrap();

        // Commit B: add another triple
        obj.store
            .add_triple(
                &sample_triple("s2", "B"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let cb = create_commit(
            &obj,
            &mut commits,
            vec![ca.commit_id.clone()],
            "commit B",
            "DGX",
        )
        .unwrap();

        // Revert B — should undo the addition of s2
        let revert_id =
            revert(&mut obj, &mut commits, &cb.commit_id, &cb.commit_id, "DGX").unwrap();

        // After revert, only s1 should exist
        assert_eq!(obj.store.len(), 1);

        // Verify revert commit exists and has correct message
        let rc = commits.get(&revert_id).unwrap();
        assert!(rc.message.starts_with("Revert:"));
        assert_eq!(rc.parent_ids, vec![cb.commit_id.clone()]);
    }

    #[test]
    fn test_revert_of_revert_restores_original() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Commit A: one triple
        obj.store
            .add_triple(
                &sample_triple("s1", "A"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let ca = create_commit(&obj, &mut commits, vec![], "commit A", "DGX").unwrap();

        // Commit B: add s2
        obj.store
            .add_triple(
                &sample_triple("s2", "B"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let cb = create_commit(
            &obj,
            &mut commits,
            vec![ca.commit_id.clone()],
            "commit B",
            "DGX",
        )
        .unwrap();

        // Revert B
        let revert_id =
            revert(&mut obj, &mut commits, &cb.commit_id, &cb.commit_id, "DGX").unwrap();
        assert_eq!(obj.store.len(), 1);

        // Revert the revert — should restore s2
        let _revert2_id = revert(&mut obj, &mut commits, &revert_id, &revert_id, "DGX").unwrap();
        assert_eq!(obj.store.len(), 2);
    }

    #[test]
    fn test_revert_merge_commit_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Create a fake merge commit with 2 parents
        obj.store
            .add_triple(
                &sample_triple("s1", "A"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c1 = create_commit(&obj, &mut commits, vec![], "c1", "DGX").unwrap();
        let c2 =
            create_commit(&obj, &mut commits, vec![c1.commit_id.clone()], "c2", "DGX").unwrap();
        let merge = create_commit(
            &obj,
            &mut commits,
            vec![c1.commit_id.clone(), c2.commit_id.clone()],
            "merge",
            "DGX",
        )
        .unwrap();

        let result = revert(
            &mut obj,
            &mut commits,
            &merge.commit_id,
            &merge.commit_id,
            "DGX",
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            RevertError::MergeCommit(_, n) => assert_eq!(n, 2),
            other => panic!("Expected MergeCommit error, got: {other:?}"),
        }
    }

    #[test]
    fn test_revert_root_commit_errors() {
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
        let c1 = create_commit(&obj, &mut commits, vec![], "root", "DGX").unwrap();

        let result = revert(&mut obj, &mut commits, &c1.commit_id, &c1.commit_id, "DGX");
        assert!(result.is_err());
        match result.unwrap_err() {
            RevertError::NoParent(_) => {}
            other => panic!("Expected NoParent error, got: {other:?}"),
        }
    }
}
