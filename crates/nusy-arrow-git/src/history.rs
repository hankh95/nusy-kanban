//! History DAG — traversal of commit parent_ids to form a directed acyclic graph.
//!
//! Provides log (ordered commit history) and ancestors (full BFS traversal).

use crate::commit::{Commit, CommitsTable};
use std::collections::{HashSet, VecDeque};

/// Return the commit log for a branch: walk parent_ids from the given commit.
///
/// Returns commits in reverse chronological order (newest first).
/// `limit` caps the number of commits returned (0 = unlimited).
pub fn log<'a>(
    commits_table: &'a CommitsTable,
    start_commit_id: &str,
    limit: usize,
) -> Vec<&'a Commit> {
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    queue.push_back(start_commit_id.to_string());

    while let Some(cid) = queue.pop_front() {
        if !visited.insert(cid.clone()) {
            continue;
        }

        if let Some(commit) = commits_table.get(&cid) {
            result.push(commit);

            if limit > 0 && result.len() >= limit {
                break;
            }

            // Enqueue parents (first parent first for linear history)
            for pid in &commit.parent_ids {
                if !visited.contains(pid.as_str()) {
                    queue.push_back(pid.clone());
                }
            }
        }
    }

    result
}

/// Return all ancestors of a commit (BFS traversal of the DAG).
///
/// Returns all reachable commits including the start commit.
pub fn ancestors<'a>(commits_table: &'a CommitsTable, commit_id: &str) -> Vec<&'a Commit> {
    log(commits_table, commit_id, 0)
}

/// Find the most recent common ancestor of two commits (for 3-way merge).
///
/// BFS from both commits simultaneously; first intersection is the common ancestor.
pub fn find_common_ancestor<'a>(
    commits_table: &'a CommitsTable,
    commit_a: &str,
    commit_b: &str,
) -> Option<&'a Commit> {
    let ancestors_a: HashSet<String> = ancestors(commits_table, commit_a)
        .iter()
        .map(|c| c.commit_id.clone())
        .collect();

    // Walk from B, find first commit that's also an ancestor of A
    let ancestors_b = ancestors(commits_table, commit_b);
    ancestors_b
        .into_iter()
        .find(|commit| ancestors_a.contains(&commit.commit_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::Commit;

    fn make_commit(id: &str, parents: Vec<&str>) -> Commit {
        Commit {
            commit_id: id.to_string(),
            parent_ids: parents.into_iter().map(String::from).collect(),
            timestamp_ms: 0,
            message: format!("commit {id}"),
            author: "test".to_string(),
        }
    }

    fn build_linear_history() -> CommitsTable {
        // c1 <- c2 <- c3
        let mut table = CommitsTable::new();
        table.append(make_commit("c1", vec![]));
        table.append(make_commit("c2", vec!["c1"]));
        table.append(make_commit("c3", vec!["c2"]));
        table
    }

    #[test]
    fn test_log_linear() {
        let table = build_linear_history();
        let result = log(&table, "c3", 0);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].commit_id, "c3");
        assert_eq!(result[1].commit_id, "c2");
        assert_eq!(result[2].commit_id, "c1");
    }

    #[test]
    fn test_log_with_limit() {
        let table = build_linear_history();
        let result = log(&table, "c3", 2);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_ancestors() {
        let table = build_linear_history();
        let result = ancestors(&table, "c3");
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_common_ancestor_linear() {
        // c1 <- c2 <- c3, common ancestor of c2 and c3 = c2
        let table = build_linear_history();
        let ca = find_common_ancestor(&table, "c2", "c3").unwrap();
        assert_eq!(ca.commit_id, "c2");
    }

    #[test]
    fn test_common_ancestor_branched() {
        // c1 <- c2 <- c3 (main)
        //    \<- c4 (feature)
        let mut table = CommitsTable::new();
        table.append(make_commit("c1", vec![]));
        table.append(make_commit("c2", vec!["c1"]));
        table.append(make_commit("c3", vec!["c2"]));
        table.append(make_commit("c4", vec!["c1"]));

        let ca = find_common_ancestor(&table, "c3", "c4").unwrap();
        assert_eq!(ca.commit_id, "c1");
    }

    #[test]
    fn test_no_common_ancestor() {
        // Two disconnected commits
        let mut table = CommitsTable::new();
        table.append(make_commit("c1", vec![]));
        table.append(make_commit("c2", vec![]));

        // c1 IS c1's ancestor set, c2 IS c2's ancestor set — no overlap
        // Actually c1 and c2 both have no parents and are distinct
        let result = find_common_ancestor(&table, "c1", "c2");
        assert!(result.is_none());
    }
}
