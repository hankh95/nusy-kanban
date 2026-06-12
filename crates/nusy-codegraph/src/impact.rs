//! Impact analysis — compute the blast radius of code changes.
//!
//! Given a `CodeDiffResult` (which nodes changed) and the CodeGraph state,
//! computes which other nodes are transitively affected via call edges,
//! which tests cover the affected area, and produces a human-readable report.

use crate::git_tools::{CodeDiffChangeType, CodeDiffResult};
use crate::schema::{CodeEdgePredicate, CodeNode, CodeNodeKind, extract_file_path, node_col};
use crate::search::{callers, tests_for, transitive_callers};
use arrow::array::{Array, RecordBatch, StringArray};
use std::collections::HashSet;

/// The blast radius of a set of code changes.
#[derive(Debug, Clone)]
pub struct ImpactReport {
    /// Nodes directly changed (from the diff).
    pub directly_changed: Vec<ChangedNode>,
    /// Nodes that call a changed node (1 hop).
    pub direct_callers: Vec<CodeNode>,
    /// Nodes reachable via transitive call chains (up to max_depth).
    pub transitive_callers: Vec<CodeNode>,
    /// Test nodes that cover any affected node.
    pub affected_tests: Vec<CodeNode>,
    /// Summary statistics.
    pub stats: ImpactStats,
}

/// A directly changed node with its change type.
#[derive(Debug, Clone)]
pub struct ChangedNode {
    pub node: CodeNode,
    pub change_type: CodeDiffChangeType,
}

/// Summary statistics for an impact report.
#[derive(Debug, Clone, Default)]
pub struct ImpactStats {
    pub directly_changed: usize,
    pub direct_callers: usize,
    pub transitive_callers: usize,
    pub affected_tests: usize,
    pub total_blast_radius: usize,
    pub files_affected: usize,
}

impl std::fmt::Display for ImpactStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} changed, {} direct callers, {} transitive callers, {} tests affected ({} total across {} files)",
            self.directly_changed,
            self.direct_callers,
            self.transitive_callers,
            self.affected_tests,
            self.total_blast_radius,
            self.files_affected,
        )
    }
}

/// Compute the impact (blast radius) of a code diff.
///
/// `diff` is the object-level diff from `codegraph_diff`.
/// `nodes_batch` is the current CodeNodes state.
/// `edges_batch` is the current CodeEdges state.
/// `max_depth` controls how deep transitive caller search goes (default 3).
pub fn impact_analysis(
    diff: &CodeDiffResult,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
    max_depth: usize,
) -> ImpactReport {
    // Build a set of changed node IDs (added + modified + removed)
    let changed_ids: HashSet<String> = diff.entries.iter().map(|e| e.node_id.clone()).collect();

    // Resolve changed nodes to CodeNode structs (skip removed — they're not in head batch)
    let node_map = build_node_map(nodes_batch);
    let directly_changed: Vec<ChangedNode> = diff
        .entries
        .iter()
        .filter_map(|entry| {
            let node = node_map.get(entry.node_id.as_str()).cloned().or_else(|| {
                // For removed nodes, synthesize a minimal CodeNode from diff entry
                if entry.change_type == CodeDiffChangeType::Removed {
                    Some(CodeNode {
                        id: entry.node_id.clone(),
                        kind: CodeNodeKind::parse(&entry.kind).unwrap_or(CodeNodeKind::Function),
                        parent_id: None,
                        name: entry.name.clone(),
                        signature: None,
                        docstring: None,
                        body_hash: None,
                        body: None,
                        loc: None,
                        cyclomatic_complexity: None,
                        coverage_pct: None,
                        last_modified: None,
                        ..Default::default()
                    })
                } else {
                    None
                }
            })?;
            Some(ChangedNode {
                node,
                change_type: entry.change_type,
            })
        })
        .collect();

    // Find direct callers of changed nodes (1 hop)
    let mut direct_caller_ids: HashSet<String> = HashSet::new();
    for entry in &diff.entries {
        if entry.change_type == CodeDiffChangeType::Removed {
            continue; // Can't find callers of removed nodes in head batch
        }
        for caller in callers(&entry.node_id, nodes_batch, edges_batch) {
            if !changed_ids.contains(&caller.id) {
                direct_caller_ids.insert(caller.id.clone());
            }
        }
    }

    // Find transitive callers (up to max_depth)
    let mut transitive_caller_ids: HashSet<String> = HashSet::new();
    for entry in &diff.entries {
        if entry.change_type == CodeDiffChangeType::Removed {
            continue;
        }
        for tc in transitive_callers(
            &entry.node_id,
            CodeEdgePredicate::Calls,
            max_depth,
            nodes_batch,
            edges_batch,
        ) {
            if !changed_ids.contains(&tc.id) && !direct_caller_ids.contains(&tc.id) {
                transitive_caller_ids.insert(tc.id.clone());
            }
        }
    }

    // Find tests that cover any affected node (changed + direct + transitive callers)
    let mut test_ids: HashSet<String> = HashSet::new();
    let all_affected: HashSet<&str> = changed_ids
        .iter()
        .chain(direct_caller_ids.iter())
        .chain(transitive_caller_ids.iter())
        .map(|s| s.as_str())
        .collect();

    for node_id in &all_affected {
        for test in tests_for(node_id, nodes_batch, edges_batch) {
            test_ids.insert(test.id.clone());
        }
    }

    // Resolve IDs to CodeNode structs
    let direct_callers: Vec<CodeNode> = direct_caller_ids
        .iter()
        .filter_map(|id| node_map.get(id.as_str()).cloned())
        .collect();
    let transitive_callers_nodes: Vec<CodeNode> = transitive_caller_ids
        .iter()
        .filter_map(|id| node_map.get(id.as_str()).cloned())
        .collect();
    let affected_tests: Vec<CodeNode> = test_ids
        .iter()
        .filter_map(|id| node_map.get(id.as_str()).cloned())
        .collect();

    // Compute stats
    let mut all_files: HashSet<String> = HashSet::new();
    for cn in &directly_changed {
        if let Some(fp) = extract_file_path(&cn.node.id) {
            all_files.insert(fp);
        }
    }
    for n in direct_callers.iter().chain(transitive_callers_nodes.iter()) {
        if let Some(fp) = extract_file_path(&n.id) {
            all_files.insert(fp);
        }
    }

    let stats = ImpactStats {
        directly_changed: directly_changed.len(),
        direct_callers: direct_callers.len(),
        transitive_callers: transitive_callers_nodes.len(),
        affected_tests: affected_tests.len(),
        total_blast_radius: directly_changed.len()
            + direct_callers.len()
            + transitive_callers_nodes.len(),
        files_affected: all_files.len(),
    };

    ImpactReport {
        directly_changed,
        direct_callers,
        transitive_callers: transitive_callers_nodes,
        affected_tests,
        stats,
    }
}

/// Format an impact report as a human-readable summary.
pub fn format_impact_report(report: &ImpactReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("## Impact Analysis\n\n{}\n\n", report.stats));

    if !report.directly_changed.is_empty() {
        out.push_str("### Directly Changed\n\n");
        for cn in &report.directly_changed {
            out.push_str(&format!(
                "  {} {} `{}`\n",
                match cn.change_type {
                    CodeDiffChangeType::Added => "+",
                    CodeDiffChangeType::Removed => "-",
                    CodeDiffChangeType::Modified => "~",
                },
                cn.node.kind,
                cn.node.name,
            ));
        }
        out.push('\n');
    }

    if !report.direct_callers.is_empty() {
        out.push_str("### Direct Callers (may need updating)\n\n");
        for n in &report.direct_callers {
            out.push_str(&format!("  {} `{}`\n", n.kind, n.name));
        }
        out.push('\n');
    }

    if !report.transitive_callers.is_empty() {
        out.push_str("### Transitive Callers (ripple effect)\n\n");
        for n in &report.transitive_callers {
            out.push_str(&format!("  {} `{}`\n", n.kind, n.name));
        }
        out.push('\n');
    }

    if !report.affected_tests.is_empty() {
        out.push_str("### Affected Tests (should be run)\n\n");
        for t in &report.affected_tests {
            out.push_str(&format!("  {} `{}`\n", t.kind, t.name));
        }
        out.push('\n');
    }

    out
}

/// Build a node_id → CodeNode map from the batch.
fn build_node_map(batch: &RecordBatch) -> std::collections::HashMap<String, CodeNode> {
    use crate::schema::CodeNodeKind;
    use arrow::array::{Float64Array, Int32Array};

    let mut map = std::collections::HashMap::new();
    if batch.num_rows() == 0 {
        return map;
    }

    let ids = batch
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id");
    let names = batch
        .column(node_col::NAME)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("name");
    let parent_ids = batch
        .column(node_col::PARENT_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("parent_id");
    let signatures = batch
        .column(node_col::SIGNATURE)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("signature");
    let docstrings = batch
        .column(node_col::DOCSTRING)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("docstring");
    let body_hashes = batch
        .column(node_col::BODY_HASH)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("body_hash");
    let locs = batch
        .column(node_col::LOC)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("loc");
    let complexities = batch
        .column(node_col::CYCLOMATIC_COMPLEXITY)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("complexity");
    let coverages = batch
        .column(node_col::COVERAGE_PCT)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("coverage");
    let kind_dict = batch
        .column(node_col::KIND)
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
        let kind_str = kind_values.value(kind_key);
        let node = CodeNode {
            id: ids.value(i).to_string(),
            kind: CodeNodeKind::parse(kind_str).unwrap_or(CodeNodeKind::Function),
            parent_id: if parent_ids.is_null(i) {
                None
            } else {
                Some(parent_ids.value(i).to_string())
            },
            name: names.value(i).to_string(),
            signature: if signatures.is_null(i) {
                None
            } else {
                Some(signatures.value(i).to_string())
            },
            docstring: if docstrings.is_null(i) {
                None
            } else {
                Some(docstrings.value(i).to_string())
            },
            body_hash: if body_hashes.is_null(i) {
                None
            } else {
                Some(body_hashes.value(i).to_string())
            },
            body: None, // Body not extracted in impact analysis
            loc: if locs.is_null(i) {
                None
            } else {
                Some(locs.value(i))
            },
            cyclomatic_complexity: if complexities.is_null(i) {
                None
            } else {
                Some(complexities.value(i))
            },
            coverage_pct: if coverages.is_null(i) {
                None
            } else {
                Some(coverages.value(i))
            },
            last_modified: None,
            ..Default::default()
        };
        map.insert(node.id.clone(), node);
    }

    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_tools::codegraph_diff;
    use crate::schema::{
        CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind, build_code_edges_batch,
        build_code_nodes_batch,
    };

    fn sample_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "func:a.py::entry".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "entry".into(),
                signature: Some("def entry()".into()),
                docstring: None,
                body_hash: Some("entry_v1".into()),
                body: None,
                loc: Some(20),
                cyclomatic_complexity: Some(3),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:a.py::process".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "process".into(),
                signature: Some("def process(data)".into()),
                docstring: None,
                body_hash: Some("proc_v1".into()),
                body: None,
                loc: Some(40),
                cyclomatic_complexity: Some(7),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:b.py::helper".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "helper".into(),
                signature: Some("def helper(x)".into()),
                docstring: None,
                body_hash: Some("help_v1".into()),
                body: None,
                loc: Some(10),
                cyclomatic_complexity: Some(1),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:b.py::unrelated".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "unrelated".into(),
                signature: None,
                docstring: None,
                body_hash: Some("unrel_v1".into()),
                body: None,
                loc: Some(5),
                cyclomatic_complexity: Some(1),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "test:tests/test_a.py::test_entry".into(),
                kind: CodeNodeKind::Test,
                parent_id: None,
                name: "test_entry".into(),
                signature: None,
                docstring: None,
                body_hash: Some("te_v1".into()),
                body: None,
                loc: Some(8),
                cyclomatic_complexity: Some(1),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    fn sample_edges() -> Vec<CodeEdge> {
        vec![
            // entry → process → helper (call chain)
            CodeEdge {
                source_id: "func:a.py::entry".into(),
                target_id: "func:a.py::process".into(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(1.0),
                commit_id: None,
            },
            CodeEdge {
                source_id: "func:a.py::process".into(),
                target_id: "func:b.py::helper".into(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(2.0),
                commit_id: None,
            },
            // test covers entry
            CodeEdge {
                source_id: "test:tests/test_a.py::test_entry".into(),
                target_id: "func:a.py::entry".into(),
                predicate: CodeEdgePredicate::Tests,
                weight: Some(1.0),
                commit_id: None,
            },
        ]
    }

    #[test]
    fn test_impact_modified_leaf() {
        // Modify helper (leaf node) — callers: process, entry (transitive)
        let base = build_code_nodes_batch(&sample_nodes()).unwrap();
        let mut head_nodes = sample_nodes();
        head_nodes[2].body_hash = Some("help_v2".into()); // modify helper
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let report = impact_analysis(&diff, &head, &edges, 3);

        assert_eq!(report.stats.directly_changed, 1);
        assert_eq!(report.directly_changed[0].node.name, "helper");

        // process calls helper directly
        assert_eq!(report.stats.direct_callers, 1);
        assert_eq!(report.direct_callers[0].name, "process");

        // entry calls process which calls helper — transitive
        // (entry is a transitive caller, not direct)
        assert!(report.stats.transitive_callers >= 1);

        assert_eq!(report.stats.affected_tests, 1);
        assert_eq!(report.affected_tests[0].name, "test_entry");
    }

    #[test]
    fn test_impact_no_changes() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();
        let diff = codegraph_diff(&nodes, &nodes).unwrap();

        let report = impact_analysis(&diff, &nodes, &edges, 3);
        assert_eq!(report.stats.directly_changed, 0);
        assert_eq!(report.stats.direct_callers, 0);
        assert_eq!(report.stats.affected_tests, 0);
    }

    #[test]
    fn test_impact_unrelated_change() {
        // Modify unrelated — no callers, no tests
        let base = build_code_nodes_batch(&sample_nodes()).unwrap();
        let mut head_nodes = sample_nodes();
        head_nodes[3].body_hash = Some("unrel_v2".into());
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let report = impact_analysis(&diff, &head, &edges, 3);

        assert_eq!(report.stats.directly_changed, 1);
        assert_eq!(report.stats.direct_callers, 0);
        assert_eq!(report.stats.transitive_callers, 0);
        assert_eq!(report.stats.affected_tests, 0);
    }

    #[test]
    fn test_impact_depth_limits_transitive() {
        let base = build_code_nodes_batch(&sample_nodes()).unwrap();
        let mut head_nodes = sample_nodes();
        head_nodes[2].body_hash = Some("help_v2".into());
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        // Depth 1: only direct callers (process), entry is at depth 2
        let diff = codegraph_diff(&base, &head).unwrap();
        let report = impact_analysis(&diff, &head, &edges, 1);
        assert_eq!(report.stats.direct_callers, 1); // process
        assert_eq!(report.stats.transitive_callers, 0); // entry excluded at depth 1
    }

    #[test]
    fn test_impact_removed_node() {
        let base = build_code_nodes_batch(&sample_nodes()).unwrap();
        // Remove helper
        let head_nodes: Vec<CodeNode> = sample_nodes()
            .into_iter()
            .filter(|n| n.name != "helper")
            .collect();
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let report = impact_analysis(&diff, &head, &edges, 3);

        assert_eq!(report.stats.directly_changed, 1);
        assert_eq!(report.directly_changed[0].node.name, "helper");
        assert_eq!(
            report.directly_changed[0].change_type,
            CodeDiffChangeType::Removed
        );
    }

    #[test]
    fn test_impact_files_affected() {
        let base = build_code_nodes_batch(&sample_nodes()).unwrap();
        let mut head_nodes = sample_nodes();
        head_nodes[2].body_hash = Some("help_v2".into()); // b.py::helper
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let report = impact_analysis(&diff, &head, &edges, 3);

        // helper is in b.py, callers (process, entry) are in a.py
        assert_eq!(report.stats.files_affected, 2);
    }

    #[test]
    fn test_format_impact_report() {
        let base = build_code_nodes_batch(&sample_nodes()).unwrap();
        let mut head_nodes = sample_nodes();
        head_nodes[2].body_hash = Some("help_v2".into());
        let head = build_code_nodes_batch(&head_nodes).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let diff = codegraph_diff(&base, &head).unwrap();
        let report = impact_analysis(&diff, &head, &edges, 3);
        let formatted = format_impact_report(&report);

        assert!(formatted.contains("## Impact Analysis"));
        assert!(formatted.contains("helper"));
        assert!(formatted.contains("Direct Callers"));
        assert!(formatted.contains("test_entry"));
    }
}
