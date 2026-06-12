use arrow::array::{RecordBatch, StringArray};
use nusy_arrow_git::{CommitsTable, DiffResult, GitObjectStore};
use std::sync::Arc;

use crate::proposals::{ProposalError, ProposalStore, Result};
use crate::schema::diff_view_schema;

/// Statistics about a proposal's changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffStats {
    pub additions: usize,
    pub deletions: usize,
    pub total: usize,
}

/// Compute the diff for a proposal as a triple-oriented Arrow RecordBatch.
///
/// Uses `diff_nondestructive` to avoid clobbering the live store state.
/// Returns a RecordBatch with columns: subject, predicate, old_object, new_object, change_type.
pub fn proposal_diff(
    proposal_store: &ProposalStore,
    proposal_id: &str,
    obj_store: &mut GitObjectStore,
    commits_table: &CommitsTable,
    refs_table: &nusy_arrow_git::RefsTable,
) -> Result<RecordBatch> {
    let source_branch = proposal_store.get_source_branch(proposal_id)?;
    let target_branch = proposal_store.get_target_branch(proposal_id)?;

    let source_commit = refs_table
        .resolve(&source_branch)
        .ok_or_else(|| ProposalError::NotFound(format!("branch not found: {source_branch}")))?
        .to_string();
    let target_commit = refs_table
        .resolve(&target_branch)
        .ok_or_else(|| ProposalError::NotFound(format!("branch not found: {target_branch}")))?
        .to_string();

    let diff_result = nusy_arrow_git::diff_nondestructive(
        obj_store,
        commits_table,
        &target_commit,
        &source_commit,
    )
    .map_err(|e| ProposalError::NotFound(format!("diff failed: {e}")))?;

    diff_result_to_batch(&diff_result)
}

/// Compute summary statistics for a proposal's diff.
pub fn proposal_stats(
    proposal_store: &ProposalStore,
    proposal_id: &str,
    obj_store: &mut GitObjectStore,
    commits_table: &CommitsTable,
    refs_table: &nusy_arrow_git::RefsTable,
) -> Result<DiffStats> {
    let source_branch = proposal_store.get_source_branch(proposal_id)?;
    let target_branch = proposal_store.get_target_branch(proposal_id)?;

    let source_commit = refs_table
        .resolve(&source_branch)
        .ok_or_else(|| ProposalError::NotFound(format!("branch not found: {source_branch}")))?
        .to_string();
    let target_commit = refs_table
        .resolve(&target_branch)
        .ok_or_else(|| ProposalError::NotFound(format!("branch not found: {target_branch}")))?
        .to_string();

    let diff_result = nusy_arrow_git::diff_nondestructive(
        obj_store,
        commits_table,
        &target_commit,
        &source_commit,
    )
    .map_err(|e| ProposalError::NotFound(format!("diff failed: {e}")))?;

    Ok(DiffStats {
        additions: diff_result.added.len(),
        deletions: diff_result.removed.len(),
        total: diff_result.total_changes(),
    })
}

/// Convert a nusy-arrow-git DiffResult into the triple-oriented diff RecordBatch.
///
/// Added entries → change_type="add", old_object=NULL
/// Removed entries → change_type="delete", new_object=NULL
///
/// Note: `modify` detection is not yet supported. A modified triple appears as
/// a delete + add pair (matching subject+predicate, different object). Modify
/// detection can be added by correlating removed/added entries with the same
/// subject+predicate key.
fn diff_result_to_batch(diff: &DiffResult) -> Result<RecordBatch> {
    let total = diff.added.len() + diff.removed.len();
    let mut subjects = Vec::with_capacity(total);
    let mut predicates = Vec::with_capacity(total);
    let mut old_objects: Vec<Option<&str>> = Vec::with_capacity(total);
    let mut new_objects: Vec<Option<&str>> = Vec::with_capacity(total);
    let mut change_types = Vec::with_capacity(total);

    for entry in &diff.added {
        subjects.push(entry.subject.as_str());
        predicates.push(entry.predicate.as_str());
        old_objects.push(None);
        new_objects.push(Some(entry.object.as_str()));
        change_types.push("add");
    }

    for entry in &diff.removed {
        subjects.push(entry.subject.as_str());
        predicates.push(entry.predicate.as_str());
        old_objects.push(Some(entry.object.as_str()));
        new_objects.push(None);
        change_types.push("delete");
    }

    let batch = RecordBatch::try_new(
        diff_view_schema(),
        vec![
            Arc::new(StringArray::from(subjects)),
            Arc::new(StringArray::from(predicates)),
            Arc::new(StringArray::from(old_objects)),
            Arc::new(StringArray::from(new_objects)),
            Arc::new(StringArray::from(change_types)),
        ],
    )?;

    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use nusy_arrow_git::DiffEntry;

    #[test]
    fn test_diff_result_to_batch_empty() {
        let diff = DiffResult {
            added: vec![],
            removed: vec![],
        };
        let batch = diff_result_to_batch(&diff).unwrap();
        assert_eq!(batch.num_rows(), 0);
        assert_eq!(batch.num_columns(), 5);
    }

    #[test]
    fn test_diff_result_to_batch_additions() {
        let diff = DiffResult {
            added: vec![DiffEntry {
                subject: "being:alpha".into(),
                predicate: "knows".into(),
                object: "calculus".into(),
                namespace: "self".into(),
                y_layer: 1,
                confidence: None,
                graph: None,
                source_document: None,
                source_chunk_id: None,
                caused_by: None,
                derived_from: None,
                consolidated_at: None,
                certifiability_class: None,
            }],
            removed: vec![],
        };
        let batch = diff_result_to_batch(&diff).unwrap();
        assert_eq!(batch.num_rows(), 1);

        let subjects = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(subjects.value(0), "being:alpha");

        let change_types = batch
            .column(4)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(change_types.value(0), "add");

        // old_object should be null for additions
        let old_objects = batch
            .column(2)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(old_objects.is_null(0));
    }

    #[test]
    fn test_diff_result_to_batch_deletions() {
        let diff = DiffResult {
            added: vec![],
            removed: vec![DiffEntry {
                subject: "being:alpha".into(),
                predicate: "believes".into(),
                object: "earth-is-flat".into(),
                namespace: "self".into(),
                y_layer: 2,
                confidence: Some(0.1),
                graph: None,
                source_document: None,
                source_chunk_id: None,
                caused_by: None,
                derived_from: None,
                consolidated_at: None,
                certifiability_class: None,
            }],
        };
        let batch = diff_result_to_batch(&diff).unwrap();
        assert_eq!(batch.num_rows(), 1);

        let change_types = batch
            .column(4)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(change_types.value(0), "delete");

        // new_object should be null for deletions
        let new_objects = batch
            .column(3)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(new_objects.is_null(0));

        // old_object should have the value
        let old_objects = batch
            .column(2)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(old_objects.value(0), "earth-is-flat");
    }

    #[test]
    fn test_diff_stats_from_result() {
        let diff = DiffResult {
            added: vec![
                DiffEntry {
                    subject: "a".into(),
                    predicate: "b".into(),
                    object: "c".into(),
                    namespace: "world".into(),
                    y_layer: 0,
                    confidence: None,
                    graph: None,
                    source_document: None,
                    source_chunk_id: None,
                    caused_by: None,
                    derived_from: None,
                    consolidated_at: None,
                    certifiability_class: None,
                },
                DiffEntry {
                    subject: "d".into(),
                    predicate: "e".into(),
                    object: "f".into(),
                    namespace: "world".into(),
                    y_layer: 0,
                    confidence: None,
                    graph: None,
                    source_document: None,
                    source_chunk_id: None,
                    caused_by: None,
                    derived_from: None,
                    consolidated_at: None,
                    certifiability_class: None,
                },
            ],
            removed: vec![DiffEntry {
                subject: "g".into(),
                predicate: "h".into(),
                object: "i".into(),
                namespace: "world".into(),
                y_layer: 0,
                confidence: None,
                graph: None,
                source_document: None,
                source_chunk_id: None,
                caused_by: None,
                derived_from: None,
                consolidated_at: None,
                certifiability_class: None,
            }],
        };
        let batch = diff_result_to_batch(&diff).unwrap();
        assert_eq!(batch.num_rows(), 3);
    }
}
