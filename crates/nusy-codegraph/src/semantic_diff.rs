//! Semantic diff — enriches object-level diffs with structural context.
//!
//! Takes a `CodeDiffResult` (which nodes changed) and produces a `SemanticDiff`
//! that adds:
//! - **Impact analysis**: which edges are affected by each change
//! - **Containment context**: parent/child relationships for changed nodes
//! - **Change classification**: API-breaking vs internal, signature vs body
//! - **Human-readable summary**: grouped by file/module for review

use crate::git_tools::{CodeDiffChangeType, CodeDiffEntry, CodeDiffResult};
use crate::schema::{CodeEdgePredicate, CodeNodeKind, edge_col, extract_file_path, node_col};
use arrow::array::{Array, RecordBatch, StringArray};
use std::collections::{HashMap, HashSet};

/// A semantically enriched diff.
#[derive(Debug, Clone)]
pub struct SemanticDiff {
    /// Enriched entries (one per changed node).
    pub entries: Vec<SemanticDiffEntry>,
    /// Edges affected by the changes (broken imports, stale calls, etc.).
    pub affected_edges: Vec<AffectedEdge>,
    /// Summary statistics.
    pub stats: DiffStats,
}

/// A single enriched diff entry.
#[derive(Debug, Clone)]
pub struct SemanticDiffEntry {
    /// The underlying object-level diff entry.
    pub diff: CodeDiffEntry,
    /// Classification of the change.
    pub classification: ChangeClassification,
    /// Parent node ID (containment).
    pub parent_id: Option<String>,
    /// Child node IDs that are contained within this changed node.
    pub children: Vec<String>,
    /// File path extracted from the node ID.
    pub file_path: Option<String>,
}

/// Classification of a code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeClassification {
    /// Public API change (function/class/method added/removed/signature changed).
    ApiBreaking,
    /// Internal implementation change (body modified, private function).
    Internal,
    /// New code (added node).
    Addition,
    /// Code removal.
    Deletion,
    /// Test change.
    TestChange,
}

impl std::fmt::Display for ChangeClassification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiBreaking => f.write_str("API-BREAKING"),
            Self::Internal => f.write_str("internal"),
            Self::Addition => f.write_str("addition"),
            Self::Deletion => f.write_str("deletion"),
            Self::TestChange => f.write_str("test"),
        }
    }
}

/// An edge affected by the diff (e.g., a call to a removed function).
#[derive(Debug, Clone)]
pub struct AffectedEdge {
    pub source_id: String,
    pub target_id: String,
    pub predicate: String,
    /// Why this edge is affected.
    pub reason: AffectedReason,
}

/// Why an edge is affected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffectedReason {
    /// The target was removed — this edge is now dangling.
    TargetRemoved,
    /// The source was removed — this edge is now dangling.
    SourceRemoved,
    /// The target was modified — callers may need updating.
    TargetModified,
    /// The source was modified — callees may be called differently.
    SourceModified,
}

impl std::fmt::Display for AffectedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TargetRemoved => f.write_str("target removed"),
            Self::SourceRemoved => f.write_str("source removed"),
            Self::TargetModified => f.write_str("target modified"),
            Self::SourceModified => f.write_str("source modified"),
        }
    }
}

/// Summary statistics for a semantic diff.
#[derive(Debug, Clone, Default)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub api_breaking: usize,
    pub test_changes: usize,
    pub affected_edges: usize,
    pub files_touched: usize,
}

impl std::fmt::Display for DiffStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} added, {} modified, {} removed ({} API-breaking), {} test changes, {} affected edges across {} files",
            self.added,
            self.modified,
            self.removed,
            self.api_breaking,
            self.test_changes,
            self.affected_edges,
            self.files_touched,
        )
    }
}

/// Compute a semantic diff from an object-level diff and the CodeGraph state.
///
/// `diff` is the object-level diff (from `codegraph_diff`).
/// `nodes_batch` is the head CodeNodes batch (current state).
/// `edges_batch` is the head CodeEdges batch (current state).
pub fn semantic_diff(
    diff: &CodeDiffResult,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> SemanticDiff {
    // Build lookup maps from the nodes batch
    let parent_map = build_parent_map(nodes_batch);
    let children_map = build_children_map(nodes_batch);
    let kind_map = build_kind_map(nodes_batch);

    // Collect changed node IDs by change type
    let mut removed_ids: HashSet<&str> = HashSet::new();
    let mut modified_ids: HashSet<&str> = HashSet::new();
    let mut added_ids: HashSet<&str> = HashSet::new();

    for entry in &diff.entries {
        match entry.change_type {
            CodeDiffChangeType::Removed => {
                removed_ids.insert(&entry.node_id);
            }
            CodeDiffChangeType::Modified => {
                modified_ids.insert(&entry.node_id);
            }
            CodeDiffChangeType::Added => {
                added_ids.insert(&entry.node_id);
            }
        }
    }

    // Enrich each diff entry
    let entries: Vec<SemanticDiffEntry> = diff
        .entries
        .iter()
        .map(|entry| {
            let parent_id = parent_map.get(entry.node_id.as_str()).cloned();
            let children = children_map
                .get(entry.node_id.as_str())
                .cloned()
                .unwrap_or_default();
            let file_path = extract_file_path(&entry.node_id);
            let classification = classify_change(entry, &kind_map);

            SemanticDiffEntry {
                diff: entry.clone(),
                classification,
                parent_id,
                children,
                file_path,
            }
        })
        .collect();

    // Find affected edges
    let affected_edges = find_affected_edges(edges_batch, &removed_ids, &modified_ids);

    // Compute stats
    let mut files: HashSet<&str> = HashSet::new();
    let mut stats = DiffStats {
        affected_edges: affected_edges.len(),
        ..Default::default()
    };

    for entry in &entries {
        match entry.diff.change_type {
            CodeDiffChangeType::Added => stats.added += 1,
            CodeDiffChangeType::Removed => stats.removed += 1,
            CodeDiffChangeType::Modified => stats.modified += 1,
        }
        if entry.classification == ChangeClassification::ApiBreaking {
            stats.api_breaking += 1;
        }
        if entry.classification == ChangeClassification::TestChange {
            stats.test_changes += 1;
        }
        if let Some(ref fp) = entry.file_path {
            files.insert(fp.as_str());
        }
    }
    stats.files_touched = files.len();

    SemanticDiff {
        entries,
        affected_edges,
        stats,
    }
}

/// Format a semantic diff as a human-readable review summary.
pub fn format_semantic_diff(diff: &SemanticDiff) -> String {
    let mut out = String::new();

    out.push_str(&format!("## Semantic Diff Summary\n\n{}\n\n", diff.stats));

    // Group entries by file
    let mut by_file: HashMap<String, Vec<&SemanticDiffEntry>> = HashMap::new();
    for entry in &diff.entries {
        let file = entry
            .file_path
            .clone()
            .unwrap_or_else(|| "(unknown)".to_string());
        by_file.entry(file).or_default().push(entry);
    }

    let mut files: Vec<&String> = by_file.keys().collect();
    files.sort();

    for file in files {
        let entries = &by_file[file];
        out.push_str(&format!("### {file}\n\n"));
        for entry in entries {
            let symbol = match entry.diff.change_type {
                CodeDiffChangeType::Added => "+",
                CodeDiffChangeType::Removed => "-",
                CodeDiffChangeType::Modified => "~",
            };
            out.push_str(&format!(
                "  {symbol} {} `{}` ({})\n",
                entry.diff.kind, entry.diff.name, entry.classification,
            ));
        }
        out.push('\n');
    }

    if !diff.affected_edges.is_empty() {
        out.push_str("### Affected Edges\n\n");
        for edge in &diff.affected_edges {
            out.push_str(&format!(
                "  ! {} → {} [{}] — {}\n",
                edge.source_id, edge.target_id, edge.predicate, edge.reason,
            ));
        }
        out.push('\n');
    }

    out
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Build node_id → parent_id map from a CodeNodes batch.
fn build_parent_map(batch: &RecordBatch) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if batch.num_rows() == 0 {
        return map;
    }

    let ids = batch
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column");
    let parents = batch
        .column(node_col::PARENT_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("parent_id column");

    for i in 0..batch.num_rows() {
        if !parents.is_null(i) {
            map.insert(ids.value(i).to_string(), parents.value(i).to_string());
        }
    }
    map
}

/// Build parent_id → [child_ids] map.
fn build_children_map(batch: &RecordBatch) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    if batch.num_rows() == 0 {
        return map;
    }

    let ids = batch
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column");
    let parents = batch
        .column(node_col::PARENT_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("parent_id column");

    for i in 0..batch.num_rows() {
        if !parents.is_null(i) {
            map.entry(parents.value(i).to_string())
                .or_default()
                .push(ids.value(i).to_string());
        }
    }
    map
}

/// Build node_id → CodeNodeKind string map.
fn build_kind_map(batch: &RecordBatch) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if batch.num_rows() == 0 {
        return map;
    }

    let ids = batch
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column");
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
        let kind_key = kind_dict.keys().value(i) as usize;
        map.insert(
            ids.value(i).to_string(),
            kind_values.value(kind_key).to_string(),
        );
    }
    map
}

/// Classify a change based on the node kind and change type.
fn classify_change(
    entry: &CodeDiffEntry,
    kind_map: &HashMap<String, String>,
) -> ChangeClassification {
    match entry.change_type {
        CodeDiffChangeType::Added => {
            if entry.kind == CodeNodeKind::Test.as_str() {
                ChangeClassification::TestChange
            } else {
                ChangeClassification::Addition
            }
        }
        CodeDiffChangeType::Removed => {
            if entry.kind == CodeNodeKind::Test.as_str() {
                ChangeClassification::TestChange
            } else {
                // Removing a public function/class/method is API-breaking
                match entry.kind.as_str() {
                    "function" | "class" | "method" => ChangeClassification::ApiBreaking,
                    _ => ChangeClassification::Deletion,
                }
            }
        }
        CodeDiffChangeType::Modified => {
            if entry.kind == CodeNodeKind::Test.as_str() {
                ChangeClassification::TestChange
            } else {
                // Modified public API items are potentially breaking
                // (we can't distinguish signature vs body changes at this level,
                // but the hash changed so the body definitely changed)
                match entry.kind.as_str() {
                    "function" | "class" | "method" => {
                        // Check if this is a nested (private) item
                        if let Some(kind) = kind_map.get(&entry.node_id) {
                            if kind == "method" {
                                // Methods inside classes — internal by default
                                ChangeClassification::Internal
                            } else {
                                ChangeClassification::ApiBreaking
                            }
                        } else {
                            ChangeClassification::ApiBreaking
                        }
                    }
                    _ => ChangeClassification::Internal,
                }
            }
        }
    }
}

/// Find edges affected by node removals and modifications.
fn find_affected_edges(
    edges_batch: &RecordBatch,
    removed_ids: &HashSet<&str>,
    modified_ids: &HashSet<&str>,
) -> Vec<AffectedEdge> {
    if edges_batch.num_rows() == 0 {
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
    let pred_col = edges_batch.column(edge_col::PREDICATE);
    let pred_dict = pred_col
        .as_any()
        .downcast_ref::<arrow::array::Int8DictionaryArray>()
        .expect("predicate dict");
    let pred_values = pred_dict
        .values()
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("pred values");

    // Check weights for tombstoned edges (weight == -1)
    let weights = edges_batch
        .column(edge_col::WEIGHT)
        .as_any()
        .downcast_ref::<arrow::array::Float32Array>()
        .expect("weight");

    let mut affected = Vec::new();

    for i in 0..edges_batch.num_rows() {
        // Skip tombstoned edges
        if !weights.is_null(i) && weights.value(i) < 0.0 {
            continue;
        }

        let source = sources.value(i);
        let target = targets.value(i);
        let pred_key = pred_dict.keys().value(i) as usize;
        let predicate = pred_values.value(pred_key).to_string();

        // Skip containment edges — they're structural, not semantic
        if predicate == CodeEdgePredicate::Contains.as_str() {
            continue;
        }

        if removed_ids.contains(target) {
            affected.push(AffectedEdge {
                source_id: source.to_string(),
                target_id: target.to_string(),
                predicate,
                reason: AffectedReason::TargetRemoved,
            });
        } else if removed_ids.contains(source) {
            affected.push(AffectedEdge {
                source_id: source.to_string(),
                target_id: target.to_string(),
                predicate,
                reason: AffectedReason::SourceRemoved,
            });
        } else if modified_ids.contains(target) {
            affected.push(AffectedEdge {
                source_id: source.to_string(),
                target_id: target.to_string(),
                predicate,
                reason: AffectedReason::TargetModified,
            });
        } else if modified_ids.contains(source) {
            affected.push(AffectedEdge {
                source_id: source.to_string(),
                target_id: target.to_string(),
                predicate,
                reason: AffectedReason::SourceModified,
            });
        }
    }

    // Sort for deterministic output
    affected.sort_by(|a, b| {
        a.source_id
            .cmp(&b.source_id)
            .then_with(|| a.target_id.cmp(&b.target_id))
    });

    affected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_tools::codegraph_diff;
    use crate::schema::{
        CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind, build_code_edges_batch,
        build_code_nodes_batch,
    };

    fn base_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "mod:brain/signal.py".into(),
                kind: CodeNodeKind::Module,
                parent_id: None,
                name: "signal".into(),
                signature: None,
                docstring: None,
                body_hash: Some("mod_hash".into()),
                body: None,
                loc: Some(100),
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/signal.py::fuse".into(),
                kind: CodeNodeKind::Function,
                parent_id: Some("mod:brain/signal.py".into()),
                name: "fuse".into(),
                signature: Some("def fuse(signals)".into()),
                docstring: Some("Fuse signals.".into()),
                body_hash: Some("fuse_v1".into()),
                body: None,
                loc: Some(30),
                cyclomatic_complexity: Some(5),
                coverage_pct: Some(0.80),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/signal.py::validate".into(),
                kind: CodeNodeKind::Function,
                parent_id: Some("mod:brain/signal.py".into()),
                name: "validate".into(),
                signature: Some("def validate(sig)".into()),
                docstring: None,
                body_hash: Some("val_v1".into()),
                body: None,
                loc: Some(15),
                cyclomatic_complexity: Some(2),
                coverage_pct: Some(0.90),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "test:brain/signal.py::test_fuse".into(),
                kind: CodeNodeKind::Test,
                parent_id: Some("mod:brain/signal.py".into()),
                name: "test_fuse".into(),
                signature: None,
                docstring: None,
                body_hash: Some("test_v1".into()),
                body: None,
                loc: Some(10),
                cyclomatic_complexity: Some(1),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    fn base_edges() -> Vec<CodeEdge> {
        vec![
            CodeEdge {
                source_id: "func:brain/signal.py::fuse".into(),
                target_id: "func:brain/signal.py::validate".into(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(2.0),
                commit_id: None,
            },
            CodeEdge {
                source_id: "test:brain/signal.py::test_fuse".into(),
                target_id: "func:brain/signal.py::fuse".into(),
                predicate: CodeEdgePredicate::Tests,
                weight: Some(1.0),
                commit_id: None,
            },
            CodeEdge {
                source_id: "mod:brain/signal.py".into(),
                target_id: "func:brain/signal.py::fuse".into(),
                predicate: CodeEdgePredicate::Contains,
                weight: None,
                commit_id: None,
            },
        ]
    }

    #[test]
    fn test_semantic_diff_no_changes() {
        let nodes = build_code_nodes_batch(&base_nodes()).unwrap();
        let edges = build_code_edges_batch(&base_edges()).unwrap();
        let diff = codegraph_diff(&nodes, &nodes).unwrap();

        let result = semantic_diff(&diff, &nodes, &edges);
        assert!(result.entries.is_empty());
        assert!(result.affected_edges.is_empty());
        assert_eq!(result.stats.files_touched, 0);
    }

    #[test]
    fn test_semantic_diff_modified_function() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut head_nodes = base_nodes();
        head_nodes[1].body_hash = Some("fuse_v2".into()); // modify fuse
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&base_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let result = semantic_diff(&diff, &head, &edges);

        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].diff.name, "fuse");
        assert_eq!(
            result.entries[0].classification,
            ChangeClassification::ApiBreaking
        );
        assert_eq!(
            result.entries[0].parent_id,
            Some("mod:brain/signal.py".to_string())
        );
        assert_eq!(
            result.entries[0].file_path,
            Some("brain/signal.py".to_string())
        );

        // fuse is modified → edges where fuse is source or target are affected
        // fuse→validate (source modified), test_fuse→fuse (target modified)
        // contains edge is skipped
        assert_eq!(result.affected_edges.len(), 2);
        assert_eq!(result.stats.api_breaking, 1);
        assert_eq!(result.stats.files_touched, 1);
    }

    #[test]
    fn test_semantic_diff_removed_function() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        // Remove validate (keep fuse, module, test)
        let head_nodes: Vec<CodeNode> = base_nodes()
            .into_iter()
            .filter(|n| n.name != "validate")
            .collect();
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&base_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let result = semantic_diff(&diff, &head, &edges);

        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].diff.name, "validate");
        assert_eq!(
            result.entries[0].classification,
            ChangeClassification::ApiBreaking
        );

        // fuse→validate edge is affected (target removed)
        assert!(
            result
                .affected_edges
                .iter()
                .any(|e| e.reason == AffectedReason::TargetRemoved)
        );
        assert_eq!(result.stats.removed, 1);
    }

    #[test]
    fn test_semantic_diff_added_function() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut head_nodes = base_nodes();
        head_nodes.push(CodeNode {
            id: "func:brain/signal.py::normalize".into(),
            kind: CodeNodeKind::Function,
            parent_id: Some("mod:brain/signal.py".into()),
            name: "normalize".into(),
            signature: Some("def normalize(data)".into()),
            docstring: None,
            body_hash: Some("norm_v1".into()),
            body: None,
            loc: Some(20),
            cyclomatic_complexity: Some(3),
            coverage_pct: None,
            last_modified: None,
            ..Default::default()
        });
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&base_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let result = semantic_diff(&diff, &head, &edges);

        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].diff.name, "normalize");
        assert_eq!(
            result.entries[0].classification,
            ChangeClassification::Addition
        );
        assert_eq!(result.stats.added, 1);
        // No affected edges — new function isn't referenced yet
        assert!(result.affected_edges.is_empty());
    }

    #[test]
    fn test_semantic_diff_test_change_classified() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut head_nodes = base_nodes();
        head_nodes[3].body_hash = Some("test_v2".into()); // modify test
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&base_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let result = semantic_diff(&diff, &head, &edges);

        assert_eq!(result.entries.len(), 1);
        assert_eq!(
            result.entries[0].classification,
            ChangeClassification::TestChange
        );
        assert_eq!(result.stats.test_changes, 1);
    }

    #[test]
    fn test_semantic_diff_containment_edges_skipped() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut head_nodes = base_nodes();
        head_nodes[1].body_hash = Some("fuse_v2".into());
        let head = build_code_nodes_batch(&head_nodes).unwrap();

        // Only containment edges
        let edges = build_code_edges_batch(&[CodeEdge {
            source_id: "mod:brain/signal.py".into(),
            target_id: "func:brain/signal.py::fuse".into(),
            predicate: CodeEdgePredicate::Contains,
            weight: None,
            commit_id: None,
        }])
        .unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let result = semantic_diff(&diff, &head, &edges);

        // Containment edges are structural, not affected
        assert!(result.affected_edges.is_empty());
    }

    #[test]
    fn test_extract_file_path() {
        assert_eq!(
            extract_file_path("func:brain/signal.py::fuse"),
            Some("brain/signal.py".to_string())
        );
        assert_eq!(
            extract_file_path("mod:brain/signal.py"),
            Some("brain/signal.py".to_string())
        );
        assert_eq!(
            extract_file_path("method:brain/store.py::Dual::get"),
            Some("brain/store.py".to_string())
        );
        assert_eq!(extract_file_path("no-prefix"), None);
    }

    #[test]
    fn test_format_semantic_diff_readable() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut head_nodes = base_nodes();
        head_nodes[1].body_hash = Some("fuse_v2".into());
        head_nodes.push(CodeNode {
            id: "func:brain/signal.py::helper".into(),
            kind: CodeNodeKind::Function,
            parent_id: Some("mod:brain/signal.py".into()),
            name: "helper".into(),
            signature: None,
            docstring: None,
            body_hash: Some("h_v1".into()),
            body: None,
            loc: Some(5),
            cyclomatic_complexity: Some(1),
            coverage_pct: None,
            last_modified: None,
            ..Default::default()
        });
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&base_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let result = semantic_diff(&diff, &head, &edges);
        let formatted = format_semantic_diff(&result);

        assert!(formatted.contains("## Semantic Diff Summary"));
        assert!(formatted.contains("brain/signal.py"));
        assert!(formatted.contains("fuse"));
        assert!(formatted.contains("helper"));
    }

    #[test]
    fn test_semantic_diff_multiple_files() {
        let base = build_code_nodes_batch(&base_nodes()).unwrap();
        let mut head_nodes = base_nodes();
        head_nodes[1].body_hash = Some("fuse_v2".into()); // brain/signal.py
        head_nodes.push(CodeNode {
            id: "func:brain/store.py::save".into(),
            kind: CodeNodeKind::Function,
            parent_id: None,
            name: "save".into(),
            signature: None,
            docstring: None,
            body_hash: Some("save_v1".into()),
            body: None,
            loc: Some(10),
            cyclomatic_complexity: Some(1),
            coverage_pct: None,
            last_modified: None,
            ..Default::default()
        });
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&base_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let result = semantic_diff(&diff, &head, &edges);

        assert_eq!(result.stats.files_touched, 2);
    }
}
