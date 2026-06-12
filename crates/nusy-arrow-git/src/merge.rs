//! Merge — 3-way merge with conflict detection.
//!
//! Finds common ancestor, computes diffs from both branches,
//! detects conflicts (same subject+predicate with different objects),
//! and produces either a clean merge or a conflict report.

use crate::checkout;
use crate::commit::{Commit, CommitError, CommitsTable, create_commit};
use crate::diff::{self, DiffEntry};
use crate::history::find_common_ancestor;
use crate::object_store::GitObjectStore;
use nusy_arrow_core::{Namespace, Triple, YLayer, col};
use std::collections::{HashMap, HashSet};

/// A merge conflict: same (subject, predicate, namespace) with different objects.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub subject: String,
    pub predicate: String,
    pub namespace: String,
    /// The object value from branch A.
    pub object_a: String,
    /// The object value from branch B.
    pub object_b: String,
}

/// Result of a merge operation.
#[derive(Debug)]
pub enum MergeResult {
    /// Clean merge — all changes applied, new commit created.
    Clean(Commit),
    /// Conflicts detected — manual resolution needed.
    Conflict(Vec<Conflict>),
    /// No common ancestor found (disconnected histories).
    NoCommonAncestor,
}

/// Errors from merge operations.
#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("Commit error: {0}")]
    Commit(#[from] CommitError),

    #[error("Store error: {0}")]
    Store(#[from] nusy_arrow_core::StoreError),
}

/// How to resolve a single conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Keep the value from branch A (current HEAD).
    KeepOurs,
    /// Keep the value from branch B (incoming).
    KeepTheirs,
    /// Keep both values as separate triples.
    KeepBoth,
    /// Drop both values (neither survives the merge).
    Drop,
}

/// Strategy for automatic conflict resolution during merge.
pub enum MergeStrategy {
    /// Default: return `MergeResult::Conflict` for manual resolution.
    Manual,
    /// Keep branch A's value for all conflicts.
    Ours,
    /// Keep branch B's value for all conflicts.
    Theirs,
    /// Compare timestamps, keep the newer value.
    LastWriterWins,
    /// Caller-defined logic: receives a `&Conflict` and returns a `Resolution`.
    Custom(Box<dyn Fn(&Conflict) -> Resolution>),
}

/// Key for conflict detection: (subject, predicate, namespace).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConflictKey {
    subject: String,
    predicate: String,
    namespace: String,
}

/// Perform a 3-way merge between two commits.
///
/// 1. Find common ancestor
/// 2. Diff ancestor→A and ancestor→B
/// 3. Detect conflicts (same subject+predicate, different objects)
/// 4. If no conflicts, apply both diffs and create merge commit
pub fn merge(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    commit_a_id: &str,
    commit_b_id: &str,
    author: &str,
) -> Result<MergeResult, MergeError> {
    // Find common ancestor
    let ancestor = match find_common_ancestor(commits_table, commit_a_id, commit_b_id) {
        Some(a) => a.commit_id.clone(),
        None => return Ok(MergeResult::NoCommonAncestor),
    };

    // Diff ancestor→A
    let diff_a = diff::diff(obj_store, commits_table, &ancestor, commit_a_id)?;
    // Diff ancestor→B
    let diff_b = diff::diff(obj_store, commits_table, &ancestor, commit_b_id)?;

    // Check for conflicts: additions in both with same (subject, predicate, namespace) but different object
    let a_adds: HashMap<ConflictKey, &DiffEntry> = diff_a
        .added
        .iter()
        .map(|e| {
            (
                ConflictKey {
                    subject: e.subject.clone(),
                    predicate: e.predicate.clone(),
                    namespace: e.namespace.clone(),
                },
                e,
            )
        })
        .collect();

    let mut conflicts = Vec::new();
    for entry_b in &diff_b.added {
        let key = ConflictKey {
            subject: entry_b.subject.clone(),
            predicate: entry_b.predicate.clone(),
            namespace: entry_b.namespace.clone(),
        };
        if let Some(entry_a) = a_adds.get(&key)
            && entry_a.object != entry_b.object
        {
            conflicts.push(Conflict {
                subject: key.subject,
                predicate: key.predicate,
                namespace: key.namespace,
                object_a: entry_a.object.clone(),
                object_b: entry_b.object.clone(),
            });
        }
    }

    if !conflicts.is_empty() {
        return Ok(MergeResult::Conflict(conflicts));
    }

    // No conflicts — delegate to shared clean merge path
    apply_clean_merge(
        obj_store,
        commits_table,
        &ancestor,
        commit_a_id,
        commit_b_id,
        &diff_a,
        &diff_b,
        author,
    )
}

/// Perform a 3-way merge with automatic conflict resolution.
///
/// Like [`merge()`], but applies a [`MergeStrategy`] when conflicts are detected.
/// With `MergeStrategy::Manual`, this behaves identically to `merge()`.
pub fn merge_with_strategy(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    commit_a_id: &str,
    commit_b_id: &str,
    author: &str,
    strategy: &MergeStrategy,
) -> Result<MergeResult, MergeError> {
    // Find common ancestor
    let ancestor = match find_common_ancestor(commits_table, commit_a_id, commit_b_id) {
        Some(a) => a.commit_id.clone(),
        None => return Ok(MergeResult::NoCommonAncestor),
    };

    // Diff ancestor→A and ancestor→B
    let diff_a = diff::diff(obj_store, commits_table, &ancestor, commit_a_id)?;
    let diff_b = diff::diff(obj_store, commits_table, &ancestor, commit_b_id)?;

    // Detect conflicts
    let a_adds: HashMap<ConflictKey, &DiffEntry> = diff_a
        .added
        .iter()
        .map(|e| {
            (
                ConflictKey {
                    subject: e.subject.clone(),
                    predicate: e.predicate.clone(),
                    namespace: e.namespace.clone(),
                },
                e,
            )
        })
        .collect();

    let b_adds: HashMap<ConflictKey, &DiffEntry> = diff_b
        .added
        .iter()
        .map(|e| {
            (
                ConflictKey {
                    subject: e.subject.clone(),
                    predicate: e.predicate.clone(),
                    namespace: e.namespace.clone(),
                },
                e,
            )
        })
        .collect();

    let mut conflicts = Vec::new();
    for (key, entry_b) in &b_adds {
        if let Some(entry_a) = a_adds.get(key)
            && entry_a.object != entry_b.object
        {
            conflicts.push(Conflict {
                subject: key.subject.clone(),
                predicate: key.predicate.clone(),
                namespace: key.namespace.clone(),
                object_a: entry_a.object.clone(),
                object_b: entry_b.object.clone(),
            });
        }
    }

    // If no conflicts, delegate to the clean merge path
    if conflicts.is_empty() {
        return apply_clean_merge(
            obj_store,
            commits_table,
            &ancestor,
            commit_a_id,
            commit_b_id,
            &diff_a,
            &diff_b,
            author,
        );
    }

    // Apply strategy
    if matches!(strategy, MergeStrategy::Manual) {
        return Ok(MergeResult::Conflict(conflicts));
    }

    // Resolve each conflict according to strategy
    let mut resolved_keys: HashMap<ConflictKey, Resolution> = HashMap::new();
    for conflict in &conflicts {
        let resolution = match strategy {
            MergeStrategy::Manual => unreachable!(),
            MergeStrategy::Ours => Resolution::KeepOurs,
            MergeStrategy::Theirs => Resolution::KeepTheirs,
            MergeStrategy::LastWriterWins => {
                // Compare consolidated_at timestamps from the diff entries
                let key = ConflictKey {
                    subject: conflict.subject.clone(),
                    predicate: conflict.predicate.clone(),
                    namespace: conflict.namespace.clone(),
                };
                let ts_a = a_adds
                    .get(&key)
                    .and_then(|e| e.consolidated_at)
                    .unwrap_or(0);
                let ts_b = b_adds
                    .get(&key)
                    .and_then(|e| e.consolidated_at)
                    .unwrap_or(0);
                if ts_a >= ts_b {
                    Resolution::KeepOurs
                } else {
                    Resolution::KeepTheirs
                }
            }
            MergeStrategy::Custom(f) => f(conflict),
        };
        resolved_keys.insert(
            ConflictKey {
                subject: conflict.subject.clone(),
                predicate: conflict.predicate.clone(),
                namespace: conflict.namespace.clone(),
            },
            resolution,
        );
    }

    // Start from ancestor state
    checkout::checkout(obj_store, commits_table, &ancestor)?;

    // Build the set of additions, applying resolutions to conflicts
    let mut all_adds: HashMap<(String, String, String, String), &DiffEntry> = HashMap::new();

    // First add all non-conflicting additions from both branches
    for entry in diff_a.added.iter().chain(diff_b.added.iter()) {
        let conflict_key = ConflictKey {
            subject: entry.subject.clone(),
            predicate: entry.predicate.clone(),
            namespace: entry.namespace.clone(),
        };

        if let Some(resolution) = resolved_keys.get(&conflict_key) {
            // This is a conflicted key — handle based on resolution
            let spo_key = (
                entry.subject.clone(),
                entry.predicate.clone(),
                entry.object.clone(),
                entry.namespace.clone(),
            );
            match resolution {
                Resolution::KeepOurs => {
                    if a_adds.contains_key(&conflict_key)
                        && a_adds.get(&conflict_key).map(|e| &e.object) == Some(&entry.object)
                    {
                        all_adds.insert(spo_key, entry);
                    }
                }
                Resolution::KeepTheirs => {
                    if b_adds.contains_key(&conflict_key)
                        && b_adds.get(&conflict_key).map(|e| &e.object) == Some(&entry.object)
                    {
                        all_adds.insert(spo_key, entry);
                    }
                }
                Resolution::KeepBoth => {
                    all_adds.insert(spo_key, entry);
                }
                Resolution::Drop => {
                    // Skip — don't add either
                }
            }
        } else {
            // Non-conflicting — add as normal (dedup by spo key, first wins)
            let key = (
                entry.subject.clone(),
                entry.predicate.clone(),
                entry.object.clone(),
                entry.namespace.clone(),
            );
            all_adds.entry(key).or_insert(entry);
        }
    }

    // Apply additions to store
    for entry in all_adds.values() {
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
            extracted_by: Some(format!("merge by {author}")),
            caused_by: entry.caused_by.clone(),
            derived_from: entry.derived_from.clone(),
            consolidated_at: entry.consolidated_at,
            certifiability_class: entry.certifiability_class.clone(),
            object_datatype: None,
        };
        obj_store.store.add_triple(&triple, ns, y_layer)?;
    }

    // Apply removals from both diffs
    apply_removals(obj_store, &diff_a, &diff_b);

    // Create merge commit
    let merge_commit = create_commit(
        obj_store,
        commits_table,
        vec![commit_a_id.to_string(), commit_b_id.to_string()],
        &format!("Merge {} into {} (resolved)", commit_b_id, commit_a_id),
        author,
    )?;

    Ok(MergeResult::Clean(merge_commit))
}

/// Apply the clean merge path (shared by `merge` and `merge_with_strategy`).
#[allow(clippy::too_many_arguments)]
fn apply_clean_merge(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    ancestor: &str,
    commit_a_id: &str,
    commit_b_id: &str,
    diff_a: &diff::DiffResult,
    diff_b: &diff::DiffResult,
    author: &str,
) -> Result<MergeResult, MergeError> {
    checkout::checkout(obj_store, commits_table, ancestor)?;

    let mut all_adds: HashMap<(String, String, String, String), &DiffEntry> = HashMap::new();
    for entry in diff_a.added.iter().chain(diff_b.added.iter()) {
        let key = (
            entry.subject.clone(),
            entry.predicate.clone(),
            entry.object.clone(),
            entry.namespace.clone(),
        );
        all_adds.entry(key).or_insert(entry);
    }

    for entry in all_adds.values() {
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
            extracted_by: Some(format!("merge by {author}")),
            caused_by: entry.caused_by.clone(),
            derived_from: entry.derived_from.clone(),
            consolidated_at: entry.consolidated_at,
            certifiability_class: entry.certifiability_class.clone(),
            object_datatype: None,
        };
        obj_store.store.add_triple(&triple, ns, y_layer)?;
    }

    apply_removals(obj_store, diff_a, diff_b);

    let merge_commit = create_commit(
        obj_store,
        commits_table,
        vec![commit_a_id.to_string(), commit_b_id.to_string()],
        &format!("Merge {} into {}", commit_b_id, commit_a_id),
        author,
    )?;

    Ok(MergeResult::Clean(merge_commit))
}

/// Apply removals from both diffs to the current store state.
fn apply_removals(
    obj_store: &mut GitObjectStore,
    diff_a: &diff::DiffResult,
    diff_b: &diff::DiffResult,
) {
    let all_removals: HashSet<(String, String, String, String)> = diff_a
        .removed
        .iter()
        .chain(diff_b.removed.iter())
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
        let batches = obj_store.store.get_namespace_batches(ns);
        let ns_str = ns.as_str().to_string();

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
                if all_removals.contains(&key) {
                    ids_to_delete.push(id_col.value(i).to_string());
                }
            }
        }

        for id in &ids_to_delete {
            let _ = obj_store.store.delete(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::create_commit;

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
    fn test_non_conflicting_merge() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Create base commit
        obj.store
            .add_triple(
                &sample_triple("base", "Base"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

        // Branch A: add triple A
        obj.store
            .add_triple(
                &sample_triple("a-only", "A"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let ca = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "branch-a",
            "DGX",
        )
        .unwrap();

        // Checkout base, add triple B
        checkout::checkout(&mut obj, &commits, &base.commit_id).unwrap();
        obj.store
            .add_triple(
                &sample_triple("b-only", "B"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let cb = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "branch-b",
            "DGX",
        )
        .unwrap();

        // Merge
        let result = merge(&mut obj, &mut commits, &ca.commit_id, &cb.commit_id, "DGX").unwrap();

        match result {
            MergeResult::Clean(mc) => {
                assert_eq!(mc.parent_ids.len(), 2);
                // After merge, store should have base + a-only + b-only
                assert!(obj.store.len() >= 3);
            }
            MergeResult::Conflict(_) => panic!("Expected clean merge"),
            MergeResult::NoCommonAncestor => panic!("Expected common ancestor"),
        }
    }

    #[test]
    fn test_conflicting_merge() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Base
        obj.store
            .add_triple(
                &sample_triple("base", "Base"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

        // Branch A: add (conflict-subj, rdf:type, TypeA)
        obj.store
            .add_triple(
                &sample_triple("conflict-subj", "TypeA"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let ca = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "branch-a",
            "DGX",
        )
        .unwrap();

        // Checkout base, Branch B: add (conflict-subj, rdf:type, TypeB)
        checkout::checkout(&mut obj, &commits, &base.commit_id).unwrap();
        obj.store
            .add_triple(
                &sample_triple("conflict-subj", "TypeB"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let cb = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "branch-b",
            "DGX",
        )
        .unwrap();

        // Merge should detect conflict
        let result = merge(&mut obj, &mut commits, &ca.commit_id, &cb.commit_id, "DGX").unwrap();

        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].subject, "conflict-subj");
                assert_eq!(conflicts[0].object_a, "TypeA");
                assert_eq!(conflicts[0].object_b, "TypeB");
            }
            _ => panic!("Expected conflict"),
        }
    }

    #[test]
    fn test_merge_commit_has_two_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        obj.store
            .add_triple(
                &sample_triple("base", "Base"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

        // Two branches with non-conflicting changes
        obj.store
            .add_triple(&sample_triple("a", "A"), Namespace::World, YLayer::Semantic)
            .unwrap();
        let ca =
            create_commit(&obj, &mut commits, vec![base.commit_id.clone()], "a", "DGX").unwrap();

        checkout::checkout(&mut obj, &commits, &base.commit_id).unwrap();
        obj.store
            .add_triple(&sample_triple("b", "B"), Namespace::Work, YLayer::Semantic)
            .unwrap();
        let cb =
            create_commit(&obj, &mut commits, vec![base.commit_id.clone()], "b", "DGX").unwrap();

        let result = merge(&mut obj, &mut commits, &ca.commit_id, &cb.commit_id, "DGX").unwrap();

        match result {
            MergeResult::Clean(mc) => {
                assert_eq!(mc.parent_ids.len(), 2);
                assert!(mc.parent_ids.contains(&ca.commit_id));
                assert!(mc.parent_ids.contains(&cb.commit_id));
            }
            _ => panic!("Expected clean merge"),
        }
    }

    /// Helper: create a conflicting scenario for strategy tests.
    /// Returns (obj_store, commits_table, commit_a_id, commit_b_id).
    fn setup_conflict_scenario() -> (GitObjectStore, CommitsTable, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        // Leak the tempdir so it lives long enough
        let tmp_path = tmp.path().to_owned();
        std::mem::forget(tmp);
        let mut obj = GitObjectStore::with_snapshot_dir(&tmp_path);
        let mut commits = CommitsTable::new();

        // Base
        obj.store
            .add_triple(
                &sample_triple("base", "Base"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

        // Branch A
        obj.store
            .add_triple(
                &sample_triple("conflict-subj", "TypeA"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let ca = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "branch-a",
            "DGX",
        )
        .unwrap();

        // Branch B
        checkout::checkout(&mut obj, &commits, &base.commit_id).unwrap();
        obj.store
            .add_triple(
                &sample_triple("conflict-subj", "TypeB"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let cb = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "branch-b",
            "DGX",
        )
        .unwrap();

        (obj, commits, ca.commit_id, cb.commit_id)
    }

    #[test]
    fn test_strategy_manual_returns_conflict() {
        let (mut obj, mut commits, ca_id, cb_id) = setup_conflict_scenario();
        let result = merge_with_strategy(
            &mut obj,
            &mut commits,
            &ca_id,
            &cb_id,
            "DGX",
            &MergeStrategy::Manual,
        )
        .unwrap();
        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].subject, "conflict-subj");
            }
            _ => panic!("Manual strategy should return Conflict"),
        }
    }

    #[test]
    fn test_strategy_ours() {
        let (mut obj, mut commits, ca_id, cb_id) = setup_conflict_scenario();
        let result = merge_with_strategy(
            &mut obj,
            &mut commits,
            &ca_id,
            &cb_id,
            "DGX",
            &MergeStrategy::Ours,
        )
        .unwrap();
        match result {
            MergeResult::Clean(_) => {
                // Store should have base + TypeA (ours), NOT TypeB
                let batches = obj
                    .store
                    .query(&nusy_arrow_core::QuerySpec {
                        subject: Some("conflict-subj".to_string()),
                        ..Default::default()
                    })
                    .unwrap();
                let total: usize = batches.iter().map(|b| b.num_rows()).sum();
                assert_eq!(total, 1, "Should have exactly one triple for conflict-subj");
                // Verify it's TypeA
                let batch = &batches[0];
                let obj_col = batch
                    .column(col::OBJECT)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .unwrap();
                assert_eq!(obj_col.value(0), "TypeA");
            }
            _ => panic!("Ours strategy should produce Clean merge"),
        }
    }

    #[test]
    fn test_strategy_theirs() {
        let (mut obj, mut commits, ca_id, cb_id) = setup_conflict_scenario();
        let result = merge_with_strategy(
            &mut obj,
            &mut commits,
            &ca_id,
            &cb_id,
            "DGX",
            &MergeStrategy::Theirs,
        )
        .unwrap();
        match result {
            MergeResult::Clean(_) => {
                let batches = obj
                    .store
                    .query(&nusy_arrow_core::QuerySpec {
                        subject: Some("conflict-subj".to_string()),
                        ..Default::default()
                    })
                    .unwrap();
                let total: usize = batches.iter().map(|b| b.num_rows()).sum();
                assert_eq!(total, 1, "Should have exactly one triple for conflict-subj");
                let batch = &batches[0];
                let obj_col = batch
                    .column(col::OBJECT)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .unwrap();
                assert_eq!(obj_col.value(0), "TypeB");
            }
            _ => panic!("Theirs strategy should produce Clean merge"),
        }
    }

    #[test]
    fn test_strategy_keep_both() {
        let (mut obj, mut commits, ca_id, cb_id) = setup_conflict_scenario();
        let result = merge_with_strategy(
            &mut obj,
            &mut commits,
            &ca_id,
            &cb_id,
            "DGX",
            &MergeStrategy::Custom(Box::new(|_| Resolution::KeepBoth)),
        )
        .unwrap();
        match result {
            MergeResult::Clean(_) => {
                let batches = obj
                    .store
                    .query(&nusy_arrow_core::QuerySpec {
                        subject: Some("conflict-subj".to_string()),
                        ..Default::default()
                    })
                    .unwrap();
                let total: usize = batches.iter().map(|b| b.num_rows()).sum();
                assert_eq!(total, 2, "KeepBoth should preserve both triples");
            }
            _ => panic!("Custom(KeepBoth) strategy should produce Clean merge"),
        }
    }

    #[test]
    fn test_strategy_drop() {
        let (mut obj, mut commits, ca_id, cb_id) = setup_conflict_scenario();
        let result = merge_with_strategy(
            &mut obj,
            &mut commits,
            &ca_id,
            &cb_id,
            "DGX",
            &MergeStrategy::Custom(Box::new(|_| Resolution::Drop)),
        )
        .unwrap();
        match result {
            MergeResult::Clean(_) => {
                let batches = obj
                    .store
                    .query(&nusy_arrow_core::QuerySpec {
                        subject: Some("conflict-subj".to_string()),
                        ..Default::default()
                    })
                    .unwrap();
                let total: usize = batches.iter().map(|b| b.num_rows()).sum();
                assert_eq!(total, 0, "Drop should remove both conflicting triples");
            }
            _ => panic!("Custom(Drop) strategy should produce Clean merge"),
        }
    }

    #[test]
    fn test_strategy_last_writer_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Base
        obj.store
            .add_triple(
                &sample_triple("base", "Base"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

        // Branch A: older timestamp
        let triple_a = Triple {
            subject: "ts-subj".to_string(),
            predicate: "rdf:type".to_string(),
            object: "OlderValue".to_string(),
            graph: None,
            confidence: Some(0.9),
            source_document: None,
            source_chunk_id: None,
            extracted_by: None,
            caused_by: None,
            derived_from: None,
            consolidated_at: Some(1000), // older
            certifiability_class: None,
            object_datatype: None,
        };
        obj.store
            .add_triple(&triple_a, Namespace::World, YLayer::Semantic)
            .unwrap();
        let ca = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "older",
            "DGX",
        )
        .unwrap();

        // Branch B: newer timestamp
        checkout::checkout(&mut obj, &commits, &base.commit_id).unwrap();
        let triple_b = Triple {
            subject: "ts-subj".to_string(),
            predicate: "rdf:type".to_string(),
            object: "NewerValue".to_string(),
            graph: None,
            confidence: Some(0.9),
            source_document: None,
            source_chunk_id: None,
            extracted_by: None,
            caused_by: None,
            derived_from: None,
            consolidated_at: Some(2000), // newer
            certifiability_class: None,
            object_datatype: None,
        };
        obj.store
            .add_triple(&triple_b, Namespace::World, YLayer::Semantic)
            .unwrap();
        let cb = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            "newer",
            "DGX",
        )
        .unwrap();

        let result = merge_with_strategy(
            &mut obj,
            &mut commits,
            &ca.commit_id,
            &cb.commit_id,
            "DGX",
            &MergeStrategy::LastWriterWins,
        )
        .unwrap();

        match result {
            MergeResult::Clean(_) => {
                let batches = obj
                    .store
                    .query(&nusy_arrow_core::QuerySpec {
                        subject: Some("ts-subj".to_string()),
                        ..Default::default()
                    })
                    .unwrap();
                let total: usize = batches.iter().map(|b| b.num_rows()).sum();
                assert_eq!(total, 1);
                let batch = &batches[0];
                let obj_col = batch
                    .column(col::OBJECT)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .unwrap();
                assert_eq!(
                    obj_col.value(0),
                    "NewerValue",
                    "LastWriterWins should pick the newer timestamp"
                );
            }
            _ => panic!("LastWriterWins should produce Clean merge"),
        }
    }

    #[test]
    fn test_strategy_custom_conditional() {
        let (mut obj, mut commits, ca_id, cb_id) = setup_conflict_scenario();
        // Custom strategy: keep ours if subject starts with "conflict", theirs otherwise
        let result = merge_with_strategy(
            &mut obj,
            &mut commits,
            &ca_id,
            &cb_id,
            "DGX",
            &MergeStrategy::Custom(Box::new(|c| {
                if c.subject.starts_with("conflict") {
                    Resolution::KeepOurs
                } else {
                    Resolution::KeepTheirs
                }
            })),
        )
        .unwrap();
        match result {
            MergeResult::Clean(_) => {
                let batches = obj
                    .store
                    .query(&nusy_arrow_core::QuerySpec {
                        subject: Some("conflict-subj".to_string()),
                        ..Default::default()
                    })
                    .unwrap();
                let batch = &batches[0];
                let obj_col = batch
                    .column(col::OBJECT)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .unwrap();
                assert_eq!(
                    obj_col.value(0),
                    "TypeA",
                    "Custom should keep ours for conflict-* subjects"
                );
            }
            _ => panic!("Custom strategy should produce Clean merge"),
        }
    }

    #[test]
    fn test_strategy_on_no_conflict_still_clean() {
        // When there are no conflicts, strategy doesn't matter — should still merge clean
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        obj.store
            .add_triple(
                &sample_triple("base", "Base"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

        // Non-conflicting branches
        obj.store
            .add_triple(
                &sample_triple("a-only", "A"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let ca =
            create_commit(&obj, &mut commits, vec![base.commit_id.clone()], "a", "DGX").unwrap();

        checkout::checkout(&mut obj, &commits, &base.commit_id).unwrap();
        obj.store
            .add_triple(
                &sample_triple("b-only", "B"),
                Namespace::Work,
                YLayer::Semantic,
            )
            .unwrap();
        let cb =
            create_commit(&obj, &mut commits, vec![base.commit_id.clone()], "b", "DGX").unwrap();

        let result = merge_with_strategy(
            &mut obj,
            &mut commits,
            &ca.commit_id,
            &cb.commit_id,
            "DGX",
            &MergeStrategy::Ours,
        )
        .unwrap();
        match result {
            MergeResult::Clean(mc) => {
                assert_eq!(mc.parent_ids.len(), 2);
                assert!(obj.store.len() >= 3);
            }
            _ => panic!("Non-conflicting merge with any strategy should be Clean"),
        }
    }
}
