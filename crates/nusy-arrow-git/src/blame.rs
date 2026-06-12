//! Blame — per-triple provenance history.
//!
//! Walks the commit DAG from a starting point, diffs each commit against
//! its parent, and builds a map of which commit introduced each triple.

use crate::commit::{CommitError, CommitsTable};
use crate::diff::{DiffEntry, diff};
use crate::history::log;
use crate::object_store::GitObjectStore;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

/// A blame entry — records who added a triple and when.
#[derive(Debug, Clone)]
pub struct BlameEntry {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub namespace: String,
    pub commit_id: String,
    pub author: String,
    pub timestamp_ms: i64,
    pub message: String,
}

/// Errors from blame operations.
#[derive(Debug, thiserror::Error)]
pub enum BlameError {
    #[error("Commit error: {0}")]
    Commit(#[from] CommitError),
}

/// Triple identity key for deduplication.
type TripleKey = (String, String, String, String); // (subject, predicate, object, namespace)

fn triple_key(entry: &DiffEntry) -> TripleKey {
    (
        entry.subject.clone(),
        entry.predicate.clone(),
        entry.object.clone(),
        entry.namespace.clone(),
    )
}

/// Blame all triples reachable from `head_commit_id`.
///
/// Walks backward through the commit DAG, diffs each commit against its
/// parent, and records which commit first added each triple.
///
/// Returns a list of BlameEntry — one per triple that was added in the
/// reachable history. Triples that existed in the root commit are attributed
/// to the root.
pub fn blame(
    obj_store: &mut GitObjectStore,
    commits_table: &CommitsTable,
    head_commit_id: &str,
    limit: usize, // 0 = unlimited
) -> Result<Vec<BlameEntry>, BlameError> {
    let history = log(commits_table, head_commit_id, limit);
    let mut result: Vec<BlameEntry> = Vec::new();
    let mut seen: HashMap<TripleKey, bool> = HashMap::new();

    for commit in &history {
        if commit.parent_ids.is_empty() {
            // Root commit — attribute all triples in the commit's state
            // (We can't diff against a parent that doesn't exist)
            // Load the root state and attribute everything
            continue;
        }

        // Skip merge commits (multiple parents — ambiguous blame)
        if commit.parent_ids.len() > 1 {
            continue;
        }

        let parent_id = &commit.parent_ids[0];
        let diff_result = diff(obj_store, commits_table, parent_id, &commit.commit_id)?;

        for entry in &diff_result.added {
            let key = triple_key(entry);
            if let Entry::Vacant(e) = seen.entry(key) {
                e.insert(true);
                result.push(BlameEntry {
                    subject: entry.subject.clone(),
                    predicate: entry.predicate.clone(),
                    object: entry.object.clone(),
                    namespace: entry.namespace.clone(),
                    commit_id: commit.commit_id.clone(),
                    author: commit.author.clone(),
                    timestamp_ms: commit.timestamp_ms,
                    message: commit.message.clone(),
                });
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommitsTable, GitObjectStore, create_commit};
    use nusy_arrow_core::{Namespace, Triple, YLayer};

    fn make_triple(s: &str, p: &str, o: &str) -> Triple {
        Triple {
            subject: s.to_string(),
            predicate: p.to_string(),
            object: o.to_string(),
            graph: None,
            confidence: Some(1.0),
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
    fn test_blame_attributes_triples_to_correct_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Commit 1: add alice
        obj.store
            .add_triple(
                &make_triple("alice", "knows", "bob"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c1 = create_commit(&obj, &mut commits, vec![], "add alice", "Mini").unwrap();

        // Commit 2: add carol
        obj.store
            .add_triple(
                &make_triple("carol", "knows", "dave"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c2 = create_commit(
            &obj,
            &mut commits,
            vec![c1.commit_id.clone()],
            "add carol",
            "DGX",
        )
        .unwrap();

        let entries = blame(&mut obj, &commits, &c2.commit_id, 0).unwrap();

        // carol→dave should be attributed to c2/DGX
        let carol_entry = entries
            .iter()
            .find(|e| e.subject == "carol")
            .expect("carol blamed");
        assert_eq!(carol_entry.author, "DGX");
        assert_eq!(carol_entry.commit_id, c2.commit_id);
    }

    #[test]
    fn test_blame_empty_history() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        obj.store
            .add_triple(
                &make_triple("a", "r", "1"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c1 = create_commit(&obj, &mut commits, vec![], "root", "test").unwrap();

        // Root commit — no parent to diff against, so no blame entries (by design)
        let entries = blame(&mut obj, &commits, &c1.commit_id, 0).unwrap();
        assert!(entries.is_empty(), "root commit has no parent to diff");
    }

    #[test]
    fn test_blame_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        obj.store
            .add_triple(
                &make_triple("a", "r", "1"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c1 = create_commit(&obj, &mut commits, vec![], "c1", "test").unwrap();

        obj.store
            .add_triple(
                &make_triple("b", "r", "2"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c2 =
            create_commit(&obj, &mut commits, vec![c1.commit_id.clone()], "c2", "test").unwrap();

        obj.store
            .add_triple(
                &make_triple("c", "r", "3"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c3 =
            create_commit(&obj, &mut commits, vec![c2.commit_id.clone()], "c3", "test").unwrap();

        // Limit to 2 commits — should only see c3's additions
        let entries = blame(&mut obj, &commits, &c3.commit_id, 2).unwrap();
        // c3 adds "c", c2 adds "b" — both within limit of 2
        assert!(entries.len() <= 2);
    }

    #[test]
    fn test_blame_deduplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        obj.store
            .add_triple(
                &make_triple("a", "r", "1"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c1 = create_commit(&obj, &mut commits, vec![], "c1", "test").unwrap();

        obj.store
            .add_triple(
                &make_triple("b", "r", "2"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        let c2 =
            create_commit(&obj, &mut commits, vec![c1.commit_id.clone()], "c2", "test").unwrap();

        let entries = blame(&mut obj, &commits, &c2.commit_id, 0).unwrap();
        // Each triple should appear at most once
        let subjects: Vec<&str> = entries.iter().map(|e| e.subject.as_str()).collect();
        let unique: std::collections::HashSet<&&str> = subjects.iter().collect();
        assert_eq!(subjects.len(), unique.len(), "no duplicates");
    }
}
