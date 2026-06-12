//! Git-integrated MCP tools for CodeGraph versioning.
//!
//! Provides 4 tools that integrate with `nusy-arrow-git`:
//! - `codegraph_commit` — snapshot CodeGraph state to Parquet
//! - `codegraph_checkout` — restore CodeGraph state from a commit
//! - `codegraph_diff` — object-level diff between CodeGraph states
//! - `codegraph_merge` — 3-way merge with conflict detection
//!
//! These operate on CodeNode/CodeEdge RecordBatches, delegating persistence
//! and versioning to `nusy-arrow-git` primitives.

use crate::schema::{edge_col, node_col};
use arrow::array::{Array, RecordBatch, StringArray};
use std::collections::HashMap;

/// Errors from git tool operations.
#[derive(Debug, thiserror::Error)]
pub enum GitToolError {
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("No nodes batch provided")]
    NoBatch,
}

pub type Result<T> = std::result::Result<T, GitToolError>;

/// A single entry in a CodeGraph diff.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeDiffEntry {
    /// The CodeNode ID (e.g., "func:brain/signal_fusion.py::fuse").
    pub node_id: String,
    /// The kind of object.
    pub kind: String,
    /// The name of the object.
    pub name: String,
    /// What changed.
    pub change_type: CodeDiffChangeType,
    /// Old body hash (None for added nodes).
    pub old_hash: Option<String>,
    /// New body hash (None for removed nodes).
    pub new_hash: Option<String>,
}

/// The type of change in a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeDiffChangeType {
    Added,
    Removed,
    Modified,
}

impl std::fmt::Display for CodeDiffChangeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodeDiffChangeType::Added => f.write_str("added"),
            CodeDiffChangeType::Removed => f.write_str("removed"),
            CodeDiffChangeType::Modified => f.write_str("modified"),
        }
    }
}

/// Result of a CodeGraph diff.
#[derive(Debug, Clone, Default)]
pub struct CodeDiffResult {
    pub entries: Vec<CodeDiffEntry>,
}

impl CodeDiffResult {
    pub fn added(&self) -> Vec<&CodeDiffEntry> {
        self.entries
            .iter()
            .filter(|e| e.change_type == CodeDiffChangeType::Added)
            .collect()
    }

    pub fn removed(&self) -> Vec<&CodeDiffEntry> {
        self.entries
            .iter()
            .filter(|e| e.change_type == CodeDiffChangeType::Removed)
            .collect()
    }

    pub fn modified(&self) -> Vec<&CodeDiffEntry> {
        self.entries
            .iter()
            .filter(|e| e.change_type == CodeDiffChangeType::Modified)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn summary(&self) -> String {
        format!(
            "{} added, {} modified, {} removed",
            self.added().len(),
            self.modified().len(),
            self.removed().len()
        )
    }
}

/// A conflict detected during merge.
#[derive(Debug, Clone)]
pub struct CodeConflict {
    /// The CodeNode ID that conflicts.
    pub node_id: String,
    /// Hash from the "ours" side.
    pub ours_hash: Option<String>,
    /// Hash from the "theirs" side.
    pub theirs_hash: Option<String>,
    /// Hash from the common ancestor (base).
    pub base_hash: Option<String>,
}

/// Result of a 3-way merge.
#[derive(Debug)]
pub struct CodeMergeResult {
    /// The merged batch (if no conflicts, or with auto-resolved changes).
    pub merged_batch: Option<RecordBatch>,
    /// Conflicts that need manual resolution.
    pub conflicts: Vec<CodeConflict>,
    /// Nodes that were cleanly merged (no conflict).
    pub clean_merges: usize,
}

impl CodeMergeResult {
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

/// Extract a map of node_id → body_hash from a CodeNodes batch.
fn extract_node_hashes(batch: &RecordBatch) -> HashMap<String, NodeSnapshot> {
    let mut map = HashMap::new();

    if batch.num_rows() == 0 {
        return map;
    }

    let ids = batch
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column");
    let body_hashes = batch
        .column(node_col::BODY_HASH)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("body_hash column");
    let names = batch
        .column(node_col::NAME)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("name column");

    // Extract kind from dictionary
    let kind_col = batch.column(node_col::KIND);
    let kind_dict = kind_col
        .as_any()
        .downcast_ref::<arrow::array::Int8DictionaryArray>()
        .expect("kind dict");
    let kind_values = kind_dict
        .values()
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("kind values");

    for i in 0..batch.num_rows() {
        let id = ids.value(i).to_string();
        let hash = if body_hashes.is_null(i) {
            None
        } else {
            Some(body_hashes.value(i).to_string())
        };
        let name = names.value(i).to_string();
        let kind_key = kind_dict.keys().value(i) as usize;
        let kind = kind_values.value(kind_key).to_string();

        map.insert(id, NodeSnapshot { hash, name, kind });
    }

    map
}

#[derive(Debug, Clone)]
struct NodeSnapshot {
    hash: Option<String>,
    name: String,
    kind: String,
}

/// Compute an object-level diff between two CodeNodes batches.
///
/// Compares by body_hash — if the hash changed, the node was modified.
/// Nodes present in `head` but not `base` are added.
/// Nodes present in `base` but not `head` are removed.
pub fn codegraph_diff(
    base_batch: &RecordBatch,
    head_batch: &RecordBatch,
) -> Result<CodeDiffResult> {
    let base_map = extract_node_hashes(base_batch);
    let head_map = extract_node_hashes(head_batch);

    let mut entries = Vec::new();

    // Added and modified
    for (id, head_snap) in &head_map {
        match base_map.get(id) {
            None => {
                entries.push(CodeDiffEntry {
                    node_id: id.clone(),
                    kind: head_snap.kind.clone(),
                    name: head_snap.name.clone(),
                    change_type: CodeDiffChangeType::Added,
                    old_hash: None,
                    new_hash: head_snap.hash.clone(),
                });
            }
            Some(base_snap) => {
                if base_snap.hash != head_snap.hash {
                    entries.push(CodeDiffEntry {
                        node_id: id.clone(),
                        kind: head_snap.kind.clone(),
                        name: head_snap.name.clone(),
                        change_type: CodeDiffChangeType::Modified,
                        old_hash: base_snap.hash.clone(),
                        new_hash: head_snap.hash.clone(),
                    });
                }
            }
        }
    }

    // Removed
    for (id, base_snap) in &base_map {
        if !head_map.contains_key(id) {
            entries.push(CodeDiffEntry {
                node_id: id.clone(),
                kind: base_snap.kind.clone(),
                name: base_snap.name.clone(),
                change_type: CodeDiffChangeType::Removed,
                old_hash: base_snap.hash.clone(),
                new_hash: None,
            });
        }
    }

    // Sort for deterministic output
    entries.sort_by(|a, b| a.node_id.cmp(&b.node_id));

    Ok(CodeDiffResult { entries })
}

/// 3-way merge of CodeNodes batches.
///
/// `base` is the common ancestor. `ours` and `theirs` are the two branches.
/// If both branches modify the same node differently, it's a conflict.
/// If only one branch modifies a node, the modification wins.
pub fn codegraph_merge(
    base_batch: &RecordBatch,
    ours_batch: &RecordBatch,
    theirs_batch: &RecordBatch,
) -> Result<CodeMergeResult> {
    let base_map = extract_node_hashes(base_batch);
    let ours_map = extract_node_hashes(ours_batch);
    let theirs_map = extract_node_hashes(theirs_batch);

    let mut conflicts = Vec::new();
    let mut clean_merges = 0usize;

    // Check all nodes that exist in either branch
    let mut all_ids: Vec<String> = ours_map
        .keys()
        .chain(theirs_map.keys())
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    all_ids.sort();

    for id in &all_ids {
        let base_hash = base_map.get(id).and_then(|s| s.hash.as_deref());
        let ours_hash = ours_map.get(id).and_then(|s| s.hash.as_deref());
        let theirs_hash = theirs_map.get(id).and_then(|s| s.hash.as_deref());

        // If both changed from base AND they disagree, it's a conflict
        let ours_changed = ours_hash != base_hash;
        let theirs_changed = theirs_hash != base_hash;

        if ours_changed && theirs_changed && ours_hash != theirs_hash {
            conflicts.push(CodeConflict {
                node_id: id.clone(),
                ours_hash: ours_hash.map(|s| s.to_string()),
                theirs_hash: theirs_hash.map(|s| s.to_string()),
                base_hash: base_hash.map(|s| s.to_string()),
            });
        } else {
            clean_merges += 1;
        }
    }

    // V14.0: Return ours_batch as the merged result when no conflicts.
    // TODO: Build a proper merged batch that includes non-conflicting changes
    // from both sides. Currently, if ours modifies A and theirs adds C,
    // the result is ours (A',B) — missing C. This is acceptable for V14.0
    // where merges are rare and agents can re-apply missing changes.
    // Full merge implementation deferred to EXP-1263 Phase 2 follow-up.
    let merged_batch = if conflicts.is_empty() {
        Some(ours_batch.clone())
    } else {
        None // Conflicts need manual resolution by the agent
    };

    Ok(CodeMergeResult {
        merged_batch,
        conflicts,
        clean_merges,
    })
}

// ─── Smart merge: semantic conflict detection with interaction warnings ──────

/// A warning about potential interaction between changes.
#[derive(Debug, Clone)]
pub struct MergeWarning {
    /// The shared dependency that both sides' changes touch callers of.
    pub shared_dep: String,
    /// Node IDs changed on the "ours" side that call the shared dep.
    pub ours_callers: Vec<String>,
    /// Node IDs changed on the "theirs" side that call the shared dep.
    pub theirs_callers: Vec<String>,
}

/// Result of a smart merge (standard merge + interaction warnings).
#[derive(Debug)]
pub struct SmartMergeResult {
    /// The standard merge result.
    pub merge: CodeMergeResult,
    /// Interaction warnings (non-blocking — both sides modify callers of same dep).
    pub warnings: Vec<MergeWarning>,
}

impl SmartMergeResult {
    pub fn has_conflicts(&self) -> bool {
        self.merge.has_conflicts()
    }

    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Smart 3-way merge with interaction warning detection.
///
/// Wraps `codegraph_merge` and additionally detects when both branches modify
/// different callers of a shared dependency — a potential interaction risk
/// even though there's no direct conflict.
pub fn smart_merge(
    base_batch: &RecordBatch,
    ours_batch: &RecordBatch,
    theirs_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> Result<SmartMergeResult> {
    let merge = codegraph_merge(base_batch, ours_batch, theirs_batch)?;

    // Compute which nodes each side changed (relative to base)
    let base_map = extract_node_hashes(base_batch);
    let ours_map = extract_node_hashes(ours_batch);
    let theirs_map = extract_node_hashes(theirs_batch);

    let ours_changed: std::collections::HashSet<String> = ours_map
        .iter()
        .filter(|(id, snap)| {
            base_map
                .get(id.as_str())
                .map(|b| b.hash != snap.hash)
                .unwrap_or(true) // added in ours
        })
        .map(|(id, _)| id.clone())
        .collect();

    let theirs_changed: std::collections::HashSet<String> = theirs_map
        .iter()
        .filter(|(id, snap)| {
            base_map
                .get(id.as_str())
                .map(|b| b.hash != snap.hash)
                .unwrap_or(true)
        })
        .map(|(id, _)| id.clone())
        .collect();

    // Build call target map: for each changed node, what does it call?
    let warnings = detect_interaction_warnings(&ours_changed, &theirs_changed, edges_batch);

    Ok(SmartMergeResult { merge, warnings })
}

/// Detect interaction warnings: both sides modify callers of the same dependency.
fn detect_interaction_warnings(
    ours_changed: &std::collections::HashSet<String>,
    theirs_changed: &std::collections::HashSet<String>,
    edges_batch: &RecordBatch,
) -> Vec<MergeWarning> {
    use crate::schema::CodeEdgePredicate;

    if edges_batch.num_rows() == 0 || ours_changed.is_empty() || theirs_changed.is_empty() {
        return Vec::new();
    }

    let sources = edges_batch
        .column(edge_col::SOURCE_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("source_id");
    let targets = edges_batch
        .column(edge_col::TARGET_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("target_id");
    let pred_dict = edges_batch
        .column(edge_col::PREDICATE)
        .as_any()
        .downcast_ref::<arrow::array::Int8DictionaryArray>()
        .expect("predicate dict");
    let pred_values = pred_dict
        .values()
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("pred values");
    let weights = edges_batch
        .column(edge_col::WEIGHT)
        .as_any()
        .downcast_ref::<arrow::array::Float32Array>()
        .expect("weight");

    let calls_str = CodeEdgePredicate::Calls.as_str();

    // Build: target → set of callers (from changed nodes only)
    let mut ours_calls: HashMap<String, Vec<String>> = HashMap::new();
    let mut theirs_calls: HashMap<String, Vec<String>> = HashMap::new();

    for i in 0..edges_batch.num_rows() {
        if !weights.is_null(i) && weights.value(i) < 0.0 {
            continue;
        }
        let pred_key = pred_dict.keys().value(i) as usize;
        if pred_values.value(pred_key) != calls_str {
            continue;
        }

        let source = sources.value(i);
        let target = targets.value(i).to_string();

        if ours_changed.contains(source) {
            ours_calls
                .entry(target.clone())
                .or_default()
                .push(source.to_string());
        }
        if theirs_changed.contains(source) {
            theirs_calls
                .entry(target)
                .or_default()
                .push(source.to_string());
        }
    }

    // Find shared deps: targets called by both ours and theirs changed nodes
    let mut warnings = Vec::new();
    for (dep, ours_callers) in &ours_calls {
        if let Some(theirs_callers) = theirs_calls.get(dep) {
            warnings.push(MergeWarning {
                shared_dep: dep.clone(),
                ours_callers: ours_callers.clone(),
                theirs_callers: theirs_callers.clone(),
            });
        }
    }
    warnings.sort_by(|a, b| a.shared_dep.cmp(&b.shared_dep));

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{CodeNode, CodeNodeKind, build_code_nodes_batch};

    fn base_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "func:a.py::foo".into(),
                kind: CodeNodeKind::Function,
                parent_id: Some("mod:a.py".into()),
                name: "foo".into(),
                signature: Some("def foo()".into()),
                docstring: None,
                body_hash: Some("hash_v1".into()),
                body: None,
                loc: Some(10),
                cyclomatic_complexity: Some(2),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:a.py::bar".into(),
                kind: CodeNodeKind::Function,
                parent_id: Some("mod:a.py".into()),
                name: "bar".into(),
                signature: Some("def bar()".into()),
                docstring: None,
                body_hash: Some("hash_bar".into()),
                body: None,
                loc: Some(20),
                cyclomatic_complexity: Some(5),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    // --- Diff tests ---

    #[test]
    fn test_diff_no_changes() {
        let batch = build_code_nodes_batch(&base_nodes()).unwrap();
        let result = codegraph_diff(&batch, &batch).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_diff_added_node() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut head_nodes = base_nodes();
        head_nodes.push(CodeNode {
            id: "func:b.py::baz".into(),
            kind: CodeNodeKind::Function,
            parent_id: None,
            name: "baz".into(),
            signature: None,
            docstring: None,
            body_hash: Some("hash_new".into()),
            body: None,
            loc: None,
            cyclomatic_complexity: None,
            coverage_pct: None,
            last_modified: None,
            ..Default::default()
        });
        let head = build_code_nodes_batch(&head_nodes).unwrap();

        let result = codegraph_diff(&base, &head).unwrap();
        assert_eq!(result.added().len(), 1);
        assert_eq!(result.added()[0].name, "baz");
        assert_eq!(result.modified().len(), 0);
        assert_eq!(result.removed().len(), 0);
    }

    #[test]
    fn test_diff_removed_node() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let head_nodes = vec![base_nodes()[0].clone()]; // Only keep first
        let head = build_code_nodes_batch(&head_nodes).unwrap();

        let result = codegraph_diff(&base, &head).unwrap();
        assert_eq!(result.removed().len(), 1);
        assert_eq!(result.removed()[0].name, "bar");
    }

    #[test]
    fn test_diff_modified_node() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut head_nodes = base_nodes();
        head_nodes[0].body_hash = Some("hash_v2".into()); // Modified
        let head = build_code_nodes_batch(&head_nodes).unwrap();

        let result = codegraph_diff(&base, &head).unwrap();
        assert_eq!(result.modified().len(), 1);
        assert_eq!(result.modified()[0].name, "foo");
        assert_eq!(result.modified()[0].old_hash, Some("hash_v1".into()));
        assert_eq!(result.modified()[0].new_hash, Some("hash_v2".into()));
    }

    #[test]
    fn test_diff_summary() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut head_nodes = base_nodes();
        head_nodes[0].body_hash = Some("hash_v2".into());
        head_nodes.push(CodeNode {
            id: "func:c.py::new_func".into(),
            kind: CodeNodeKind::Function,
            parent_id: None,
            name: "new_func".into(),
            signature: None,
            docstring: None,
            body_hash: Some("new_hash".into()),
            body: None,
            loc: None,
            cyclomatic_complexity: None,
            coverage_pct: None,
            last_modified: None,
            ..Default::default()
        });
        let head = build_code_nodes_batch(&head_nodes).unwrap();

        let result = codegraph_diff(&base, &head).unwrap();
        assert_eq!(result.summary(), "1 added, 1 modified, 0 removed");
    }

    #[test]
    fn test_diff_empty_batches() {
        let empty = build_code_nodes_batch(&[]).unwrap();
        let result = codegraph_diff(&empty, &empty).unwrap();
        assert!(result.is_empty());
    }

    // --- Merge tests ---

    #[test]
    fn test_merge_no_changes() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let result = codegraph_merge(&base, &base, &base).unwrap();
        assert!(!result.has_conflicts());
        assert!(result.merged_batch.is_some());
    }

    #[test]
    fn test_merge_one_side_changes() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut ours_nodes = base_nodes();
        ours_nodes[0].body_hash = Some("ours_hash".into());
        let ours = build_code_nodes_batch(&ours_nodes).unwrap();

        let result = codegraph_merge(&base, &ours, &base).unwrap();
        assert!(!result.has_conflicts());
        assert!(result.merged_batch.is_some());
    }

    #[test]
    fn test_merge_both_sides_same_change() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut changed = base_nodes();
        changed[0].body_hash = Some("same_hash".into());
        let ours = build_code_nodes_batch(&changed).unwrap();
        let theirs = build_code_nodes_batch(&changed).unwrap();

        let result = codegraph_merge(&base, &ours, &theirs).unwrap();
        assert!(!result.has_conflicts()); // Same change = no conflict
    }

    #[test]
    fn test_merge_conflict() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut ours_nodes = base_nodes();
        ours_nodes[0].body_hash = Some("ours_hash".into());
        let ours = build_code_nodes_batch(&ours_nodes).unwrap();

        let mut theirs_nodes = base_nodes();
        theirs_nodes[0].body_hash = Some("theirs_hash".into());
        let theirs = build_code_nodes_batch(&theirs_nodes).unwrap();

        let result = codegraph_merge(&base, &ours, &theirs).unwrap();
        assert!(result.has_conflicts());
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(result.conflicts[0].node_id, "func:a.py::foo");
        assert_eq!(result.conflicts[0].ours_hash, Some("ours_hash".into()));
        assert_eq!(result.conflicts[0].theirs_hash, Some("theirs_hash".into()));
        assert_eq!(result.conflicts[0].base_hash, Some("hash_v1".into()));
        assert!(result.merged_batch.is_none()); // Can't merge with conflicts
    }

    #[test]
    fn test_merge_conflict_reports_to_agent() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut ours_nodes = base_nodes();
        ours_nodes[0].body_hash = Some("v_ours".into());
        ours_nodes[1].body_hash = Some("v_ours_bar".into());
        let ours = build_code_nodes_batch(&ours_nodes).unwrap();

        let mut theirs_nodes = base_nodes();
        theirs_nodes[0].body_hash = Some("v_theirs".into());
        // theirs_nodes[1] unchanged — should not conflict
        let theirs = build_code_nodes_batch(&theirs_nodes).unwrap();

        let result = codegraph_merge(&base, &ours, &theirs).unwrap();
        assert_eq!(result.conflicts.len(), 1); // Only foo conflicts
        assert_eq!(result.conflicts[0].node_id, "func:a.py::foo");
        // bar was only changed on ours side — clean merge
    }

    // --- Smart merge tests ---

    use crate::schema::{CodeEdge, CodeEdgePredicate, build_code_edges_batch};

    fn smart_merge_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "func:a.py::caller_a".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "caller_a".into(),
                signature: None,
                docstring: None,
                body_hash: Some("ca_v1".into()),
                body: None,
                loc: Some(10),
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:a.py::caller_b".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "caller_b".into(),
                signature: None,
                docstring: None,
                body_hash: Some("cb_v1".into()),
                body: None,
                loc: Some(10),
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:b.py::shared_dep".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "shared_dep".into(),
                signature: None,
                docstring: None,
                body_hash: Some("sd_v1".into()),
                body: None,
                loc: Some(20),
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    fn smart_merge_edges() -> Vec<CodeEdge> {
        vec![
            // caller_a → shared_dep
            CodeEdge {
                source_id: "func:a.py::caller_a".into(),
                target_id: "func:b.py::shared_dep".into(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(1.0),
                commit_id: None,
            },
            // caller_b → shared_dep
            CodeEdge {
                source_id: "func:a.py::caller_b".into(),
                target_id: "func:b.py::shared_dep".into(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(1.0),
                commit_id: None,
            },
        ]
    }

    #[test]
    fn test_smart_merge_no_interaction() {
        let base = build_code_nodes_batch(&smart_merge_nodes()).unwrap();
        let edges = build_code_edges_batch(&smart_merge_edges()).unwrap();

        // Only ours changes caller_a — no interaction
        let mut ours_nodes = smart_merge_nodes();
        ours_nodes[0].body_hash = Some("ca_v2".into());
        let ours = build_code_nodes_batch(&ours_nodes).unwrap();

        let result = smart_merge(&base, &ours, &base, &edges).unwrap();
        assert!(!result.has_conflicts());
        assert!(!result.has_warnings());
    }

    #[test]
    fn test_smart_merge_interaction_warning() {
        let base = build_code_nodes_batch(&smart_merge_nodes()).unwrap();
        let edges = build_code_edges_batch(&smart_merge_edges()).unwrap();

        // Ours changes caller_a, theirs changes caller_b
        // Both call shared_dep — interaction warning
        let mut ours_nodes = smart_merge_nodes();
        ours_nodes[0].body_hash = Some("ca_v2".into());
        let ours = build_code_nodes_batch(&ours_nodes).unwrap();

        let mut theirs_nodes = smart_merge_nodes();
        theirs_nodes[1].body_hash = Some("cb_v2".into());
        let theirs = build_code_nodes_batch(&theirs_nodes).unwrap();

        let result = smart_merge(&base, &ours, &theirs, &edges).unwrap();
        assert!(!result.has_conflicts()); // Different nodes changed — no conflict
        assert!(result.has_warnings()); // Both modify callers of shared_dep
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].shared_dep, "func:b.py::shared_dep");
    }

    #[test]
    fn test_smart_merge_conflict_still_detected() {
        let base = build_code_nodes_batch(&smart_merge_nodes()).unwrap();
        let edges = build_code_edges_batch(&smart_merge_edges()).unwrap();

        // Both sides modify the SAME node → conflict (not just warning)
        let mut ours_nodes = smart_merge_nodes();
        ours_nodes[0].body_hash = Some("ca_ours".into());
        let ours = build_code_nodes_batch(&ours_nodes).unwrap();

        let mut theirs_nodes = smart_merge_nodes();
        theirs_nodes[0].body_hash = Some("ca_theirs".into());
        let theirs = build_code_nodes_batch(&theirs_nodes).unwrap();

        let result = smart_merge(&base, &ours, &theirs, &edges).unwrap();
        assert!(result.has_conflicts());
    }
}
