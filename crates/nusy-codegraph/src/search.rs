//! Code search — graph-aware querying of CodeNodes and CodeEdges.
//!
//! Extends `mcp_tools::codegraph_query_objects` with graph-traversal capabilities:
//! - **Edge-based search**: find callers of, callees of, importers of, tests for
//! - **Pattern search**: case-insensitive substring match on name, signature, docstring
//! - **Dependency search**: find all transitive dependencies (callers N levels deep)
//! - **Composite search**: combine structural + graph criteria

use crate::schema::{
    CodeEdgePredicate, CodeNode, CodeNodeKind, edge_col, extract_file_path, node_col,
};
use arrow::array::{Array, Float32Array, Float64Array, Int32Array, RecordBatch, StringArray};
use std::collections::{HashMap, HashSet, VecDeque};

/// A search query that combines structural and graph criteria.
#[derive(Debug, Default, Clone)]
pub struct CodeSearch {
    /// Filter by node kind.
    pub kind: Option<CodeNodeKind>,
    /// Regex pattern to match against name.
    pub name_pattern: Option<String>,
    /// Regex pattern to match against signature.
    pub signature_pattern: Option<String>,
    /// Regex pattern to match against docstring.
    pub docstring_pattern: Option<String>,
    /// Substring pattern to match against body text.
    pub body_pattern: Option<String>,
    /// Filter by file path prefix.
    pub file_prefix: Option<String>,
    /// Filter by minimum LOC.
    pub min_loc: Option<i32>,
    /// Filter by minimum cyclomatic complexity.
    pub min_complexity: Option<i32>,
    /// Filter by maximum coverage percentage.
    pub max_coverage: Option<f64>,
    /// Maximum number of results.
    pub limit: Option<usize>,
}

/// Result of a code search.
#[derive(Debug)]
pub struct SearchResult {
    /// Matching nodes.
    pub nodes: Vec<CodeNode>,
    /// Total nodes scanned.
    pub total_scanned: usize,
}

/// Find nodes matching a `CodeSearch` query.
///
/// This is a richer alternative to `codegraph_query_objects` that supports
/// regex patterns and file path filtering.
pub fn search_nodes(batch: &RecordBatch, query: &CodeSearch) -> SearchResult {
    if batch.num_rows() == 0 {
        return SearchResult {
            nodes: Vec::new(),
            total_scanned: 0,
        };
    }

    let ids = col_str(batch, node_col::ID);
    let names = col_str(batch, node_col::NAME);
    let parent_ids = col_str(batch, node_col::PARENT_ID);
    let signatures = col_str(batch, node_col::SIGNATURE);
    let docstrings = col_str(batch, node_col::DOCSTRING);
    let body_hashes = col_str(batch, node_col::BODY_HASH);
    let bodies = batch
        .column(node_col::BODY)
        .as_any()
        .downcast_ref::<arrow::array::LargeStringArray>()
        .expect("body column");
    let locs = col_i32(batch, node_col::LOC);
    let complexities = col_i32(batch, node_col::CYCLOMATIC_COMPLEXITY);
    let coverages = col_f64(batch, node_col::COVERAGE_PCT);
    let (kind_dict, kind_values) = col_dict_str(batch, node_col::KIND);

    let total_scanned = batch.num_rows();
    let mut matched = Vec::new();

    for i in 0..total_scanned {
        let kind_key = kind_dict.keys().value(i) as usize;
        let kind_str = kind_values.value(kind_key);
        let name = names.value(i);
        let id = ids.value(i);

        // Kind filter
        if let Some(ref k) = query.kind
            && kind_str != k.as_str()
        {
            continue;
        }

        // Name pattern (case-insensitive substring)
        if let Some(ref pat) = query.name_pattern
            && !name.to_lowercase().contains(&pat.to_lowercase())
        {
            continue;
        }

        // Signature pattern
        if let Some(ref pat) = query.signature_pattern
            && (signatures.is_null(i)
                || !signatures
                    .value(i)
                    .to_lowercase()
                    .contains(&pat.to_lowercase()))
        {
            continue;
        }

        // Docstring pattern
        if let Some(ref pat) = query.docstring_pattern
            && (docstrings.is_null(i)
                || !docstrings
                    .value(i)
                    .to_lowercase()
                    .contains(&pat.to_lowercase()))
        {
            continue;
        }

        // Body pattern (case-insensitive substring on body text)
        if let Some(ref pat) = query.body_pattern
            && (bodies.is_null(i) || !bodies.value(i).to_lowercase().contains(&pat.to_lowercase()))
        {
            continue;
        }

        // File prefix (extracted from node ID)
        if let Some(ref prefix) = query.file_prefix {
            let file_path = extract_file_path(id);
            match file_path {
                Some(fp) if fp.starts_with(prefix.as_str()) => {}
                _ => continue,
            }
        }

        // Metric filters
        if let Some(min) = query.min_loc
            && (locs.is_null(i) || locs.value(i) < min)
        {
            continue;
        }
        if let Some(min) = query.min_complexity
            && (complexities.is_null(i) || complexities.value(i) < min)
        {
            continue;
        }
        if let Some(max) = query.max_coverage
            && (coverages.is_null(i) || coverages.value(i) > max)
        {
            continue;
        }

        let node = extract_node(
            i,
            ids,
            kind_str,
            parent_ids,
            names,
            signatures,
            docstrings,
            body_hashes,
            locs,
            complexities,
            coverages,
        );
        matched.push(node);

        if let Some(limit) = query.limit
            && matched.len() >= limit
        {
            break;
        }
    }

    SearchResult {
        nodes: matched,
        total_scanned,
    }
}

/// Find all nodes that have an edge of `predicate` type pointing TO `target_id`.
///
/// Example: `find_sources("func:a.py::foo", Calls, ...)` finds all callers of foo.
pub fn find_sources(
    target_id: &str,
    predicate: CodeEdgePredicate,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> Vec<CodeNode> {
    let source_ids =
        collect_edge_endpoints(edges_batch, target_id, predicate, EdgeDirection::Incoming);
    let refs: Vec<&str> = source_ids.iter().map(|s| s.as_str()).collect();
    resolve_node_ids(&refs, nodes_batch)
}

/// Find all nodes that `source_id` has an edge of `predicate` type pointing TO.
///
/// Example: `find_targets("func:a.py::foo", Calls, ...)` finds all functions foo calls.
pub fn find_targets(
    source_id: &str,
    predicate: CodeEdgePredicate,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> Vec<CodeNode> {
    let target_ids =
        collect_edge_endpoints(edges_batch, source_id, predicate, EdgeDirection::Outgoing);
    let refs: Vec<&str> = target_ids.iter().map(|s| s.as_str()).collect();
    resolve_node_ids(&refs, nodes_batch)
}

/// Find all callers of a function/method (convenience wrapper).
pub fn callers(
    node_id: &str,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> Vec<CodeNode> {
    find_sources(node_id, CodeEdgePredicate::Calls, nodes_batch, edges_batch)
}

/// Find all callees of a function/method (convenience wrapper).
pub fn callees(
    node_id: &str,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> Vec<CodeNode> {
    find_targets(node_id, CodeEdgePredicate::Calls, nodes_batch, edges_batch)
}

/// Find tests that cover a node (convenience wrapper).
pub fn tests_for(
    node_id: &str,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> Vec<CodeNode> {
    find_sources(node_id, CodeEdgePredicate::Tests, nodes_batch, edges_batch)
}

/// Find all children (contained nodes) of a given node.
pub fn children_of(
    node_id: &str,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> Vec<CodeNode> {
    find_targets(
        node_id,
        CodeEdgePredicate::Contains,
        nodes_batch,
        edges_batch,
    )
}

/// Transitive dependency search — find all nodes reachable from `start_id`
/// following edges of type `predicate`, up to `max_depth` hops.
///
/// Returns nodes in breadth-first order (excluding the start node).
pub fn transitive_deps(
    start_id: &str,
    predicate: CodeEdgePredicate,
    max_depth: usize,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> Vec<CodeNode> {
    // Build adjacency list from edges
    let adj = build_adjacency(edges_batch, predicate, EdgeDirection::Outgoing);

    // BFS
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(start_id.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((start_id.to_string(), 0));
    let mut reachable: Vec<String> = Vec::new();

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(neighbors) = adj.get(&current) {
            for neighbor in neighbors {
                if visited.insert(neighbor.clone()) {
                    reachable.push(neighbor.clone());
                    queue.push_back((neighbor.clone(), depth + 1));
                }
            }
        }
    }

    resolve_node_ids(
        &reachable.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        nodes_batch,
    )
}

/// Reverse transitive search — find all nodes that can reach `target_id`
/// following edges of type `predicate` in reverse, up to `max_depth` hops.
///
/// Example: `transitive_callers("func:a.py::foo", 3, ...)` finds all functions
/// that directly or indirectly call foo, up to 3 levels deep.
pub fn transitive_callers(
    target_id: &str,
    predicate: CodeEdgePredicate,
    max_depth: usize,
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
) -> Vec<CodeNode> {
    let adj = build_adjacency(edges_batch, predicate, EdgeDirection::Incoming);

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(target_id.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((target_id.to_string(), 0));
    let mut reachable: Vec<String> = Vec::new();

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(neighbors) = adj.get(&current) {
            for neighbor in neighbors {
                if visited.insert(neighbor.clone()) {
                    reachable.push(neighbor.clone());
                    queue.push_back((neighbor.clone(), depth + 1));
                }
            }
        }
    }

    resolve_node_ids(
        &reachable.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        nodes_batch,
    )
}

// ── Internal helpers ────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum EdgeDirection {
    /// Follow edges where the node is the target (find sources).
    Incoming,
    /// Follow edges where the node is the source (find targets).
    Outgoing,
}

/// Collect endpoint IDs from edges matching a specific node and predicate.
fn collect_edge_endpoints(
    edges_batch: &RecordBatch,
    node_id: &str,
    predicate: CodeEdgePredicate,
    direction: EdgeDirection,
) -> Vec<String> {
    if edges_batch.num_rows() == 0 {
        return Vec::new();
    }

    let sources = col_str(edges_batch, edge_col::SOURCE_ID);
    let targets = col_str(edges_batch, edge_col::TARGET_ID);
    let (pred_dict, pred_values) = col_dict_str(edges_batch, edge_col::PREDICATE);
    let weights = edges_batch
        .column(edge_col::WEIGHT)
        .as_any()
        .downcast_ref::<Float32Array>()
        .expect("weight");

    let pred_str = predicate.as_str();
    let mut results = Vec::new();

    for i in 0..edges_batch.num_rows() {
        // Skip tombstoned edges
        if !weights.is_null(i) && weights.value(i) < 0.0 {
            continue;
        }

        let pred_key = pred_dict.keys().value(i) as usize;
        if pred_values.value(pred_key) != pred_str {
            continue;
        }

        match direction {
            EdgeDirection::Incoming => {
                if targets.value(i) == node_id {
                    results.push(sources.value(i).to_string());
                }
            }
            EdgeDirection::Outgoing => {
                if sources.value(i) == node_id {
                    results.push(targets.value(i).to_string());
                }
            }
        }
    }

    results
}

/// Build an adjacency list from edges of a given predicate.
fn build_adjacency(
    edges_batch: &RecordBatch,
    predicate: CodeEdgePredicate,
    direction: EdgeDirection,
) -> HashMap<String, Vec<String>> {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();

    if edges_batch.num_rows() == 0 {
        return adj;
    }

    let sources = col_str(edges_batch, edge_col::SOURCE_ID);
    let targets = col_str(edges_batch, edge_col::TARGET_ID);
    let (pred_dict, pred_values) = col_dict_str(edges_batch, edge_col::PREDICATE);
    let weights = edges_batch
        .column(edge_col::WEIGHT)
        .as_any()
        .downcast_ref::<Float32Array>()
        .expect("weight");

    let pred_str = predicate.as_str();

    for i in 0..edges_batch.num_rows() {
        if !weights.is_null(i) && weights.value(i) < 0.0 {
            continue;
        }

        let pred_key = pred_dict.keys().value(i) as usize;
        if pred_values.value(pred_key) != pred_str {
            continue;
        }

        match direction {
            EdgeDirection::Outgoing => {
                adj.entry(sources.value(i).to_string())
                    .or_default()
                    .push(targets.value(i).to_string());
            }
            EdgeDirection::Incoming => {
                adj.entry(targets.value(i).to_string())
                    .or_default()
                    .push(sources.value(i).to_string());
            }
        }
    }

    adj
}

/// Resolve a list of node IDs to CodeNode structs from the batch.
fn resolve_node_ids(ids: &[&str], batch: &RecordBatch) -> Vec<CodeNode> {
    if batch.num_rows() == 0 || ids.is_empty() {
        return Vec::new();
    }

    let id_set: HashSet<&str> = ids.iter().copied().collect();
    let batch_ids = col_str(batch, node_col::ID);
    let names = col_str(batch, node_col::NAME);
    let parent_ids = col_str(batch, node_col::PARENT_ID);
    let signatures = col_str(batch, node_col::SIGNATURE);
    let docstrings = col_str(batch, node_col::DOCSTRING);
    let body_hashes = col_str(batch, node_col::BODY_HASH);
    let locs = col_i32(batch, node_col::LOC);
    let complexities = col_i32(batch, node_col::CYCLOMATIC_COMPLEXITY);
    let coverages = col_f64(batch, node_col::COVERAGE_PCT);
    let (kind_dict, kind_values) = col_dict_str(batch, node_col::KIND);

    let mut result = Vec::new();
    for i in 0..batch.num_rows() {
        let id = batch_ids.value(i);
        if id_set.contains(id) {
            let kind_key = kind_dict.keys().value(i) as usize;
            let kind_str = kind_values.value(kind_key);
            result.push(extract_node(
                i,
                batch_ids,
                kind_str,
                parent_ids,
                names,
                signatures,
                docstrings,
                body_hashes,
                locs,
                complexities,
                coverages,
            ));
        }
    }
    result
}

// ── Column accessor helpers ─────────────────────────────────────────────────

fn col_str(batch: &RecordBatch, idx: usize) -> &StringArray {
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("string column")
}

fn col_i32(batch: &RecordBatch, idx: usize) -> &Int32Array {
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("int32 column")
}

fn col_f64(batch: &RecordBatch, idx: usize) -> &Float64Array {
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("float64 column")
}

fn col_dict_str(
    batch: &RecordBatch,
    idx: usize,
) -> (&arrow::array::Int8DictionaryArray, &StringArray) {
    let dict = batch
        .column(idx)
        .as_any()
        .downcast_ref::<arrow::array::Int8DictionaryArray>()
        .expect("dict column");
    let values = dict
        .values()
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("dict values");
    (dict, values)
}

#[allow(clippy::too_many_arguments)]
fn extract_node(
    i: usize,
    ids: &StringArray,
    kind_str: &str,
    parent_ids: &StringArray,
    names: &StringArray,
    signatures: &StringArray,
    docstrings: &StringArray,
    body_hashes: &StringArray,
    locs: &Int32Array,
    complexities: &Int32Array,
    coverages: &Float64Array,
) -> CodeNode {
    CodeNode {
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
        body: None, // Body not extracted in search (use dedicated body query)
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind, build_code_edges_batch,
        build_code_nodes_batch,
    };

    fn sample_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "mod:brain/signal.py".into(),
                kind: CodeNodeKind::Module,
                parent_id: None,
                name: "signal".into(),
                signature: None,
                docstring: Some("Signal processing module.".into()),
                body_hash: Some("mod_h".into()),
                body: None,
                loc: Some(200),
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
                signature: Some("def fuse(signals: List) -> Decision".into()),
                docstring: Some("Fuse cognitive signals.".into()),
                body_hash: Some("fuse_h".into()),
                body: None,
                loc: Some(42),
                cyclomatic_complexity: Some(8),
                coverage_pct: Some(0.85),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/signal.py::validate".into(),
                kind: CodeNodeKind::Function,
                parent_id: Some("mod:brain/signal.py".into()),
                name: "validate".into(),
                signature: Some("def validate(sig: Signal) -> bool".into()),
                docstring: None,
                body_hash: Some("val_h".into()),
                body: None,
                loc: Some(15),
                cyclomatic_complexity: Some(2),
                coverage_pct: Some(0.95),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/store.py::promote".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "promote".into(),
                signature: Some("def promote(item) -> None".into()),
                docstring: Some("Promote to long-term.".into()),
                body_hash: Some("prom_h".into()),
                body: None,
                loc: Some(30),
                cyclomatic_complexity: Some(4),
                coverage_pct: Some(0.60),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "test:brain/signal.py::test_fuse".into(),
                kind: CodeNodeKind::Test,
                parent_id: None,
                name: "test_fuse".into(),
                signature: None,
                docstring: None,
                body_hash: Some("test_h".into()),
                body: None,
                loc: Some(10),
                cyclomatic_complexity: Some(1),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/nn.py::activate".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "activate".into(),
                signature: Some("def activate(x: Tensor) -> Tensor".into()),
                docstring: Some("Apply activation function.".into()),
                body_hash: Some("act_h".into()),
                body: Some("def activate(x):\n    return softmax(x)".into()),
                loc: Some(5),
                cyclomatic_complexity: Some(1),
                coverage_pct: Some(0.90),
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    fn sample_edges() -> Vec<CodeEdge> {
        vec![
            // fuse calls validate
            CodeEdge {
                source_id: "func:brain/signal.py::fuse".into(),
                target_id: "func:brain/signal.py::validate".into(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(2.0),
                commit_id: None,
            },
            // fuse calls promote
            CodeEdge {
                source_id: "func:brain/signal.py::fuse".into(),
                target_id: "func:brain/store.py::promote".into(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(1.0),
                commit_id: None,
            },
            // validate calls promote
            CodeEdge {
                source_id: "func:brain/signal.py::validate".into(),
                target_id: "func:brain/store.py::promote".into(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(1.0),
                commit_id: None,
            },
            // test_fuse tests fuse
            CodeEdge {
                source_id: "test:brain/signal.py::test_fuse".into(),
                target_id: "func:brain/signal.py::fuse".into(),
                predicate: CodeEdgePredicate::Tests,
                weight: Some(1.0),
                commit_id: None,
            },
            // module contains fuse
            CodeEdge {
                source_id: "mod:brain/signal.py".into(),
                target_id: "func:brain/signal.py::fuse".into(),
                predicate: CodeEdgePredicate::Contains,
                weight: None,
                commit_id: None,
            },
            // module contains validate
            CodeEdge {
                source_id: "mod:brain/signal.py".into(),
                target_id: "func:brain/signal.py::validate".into(),
                predicate: CodeEdgePredicate::Contains,
                weight: None,
                commit_id: None,
            },
        ]
    }

    // ── search_nodes tests ──────────────────────────────────────────────

    #[test]
    fn test_search_all() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = search_nodes(&nodes, &CodeSearch::default());
        assert_eq!(result.total_scanned, 6);
        assert_eq!(result.nodes.len(), 6);
    }

    #[test]
    fn test_search_by_kind() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                kind: Some(CodeNodeKind::Function),
                ..Default::default()
            },
        );
        assert_eq!(result.nodes.len(), 4); // fuse, validate, promote, activate
    }

    #[test]
    fn test_search_by_name_pattern() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                name_pattern: Some("fus".into()),
                ..Default::default()
            },
        );
        assert_eq!(result.nodes.len(), 2); // fuse + test_fuse
    }

    #[test]
    fn test_search_by_signature_pattern() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                signature_pattern: Some("-> bool".into()),
                ..Default::default()
            },
        );
        assert_eq!(result.nodes.len(), 1); // only validate returns bool
        assert_eq!(result.nodes[0].name, "validate");
    }

    #[test]
    fn test_search_by_docstring_pattern() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                docstring_pattern: Some("cognitive".into()),
                ..Default::default()
            },
        );
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "fuse");
    }

    #[test]
    fn test_search_by_body_pattern() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        // "softmax" appears in the body of the activate function
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                body_pattern: Some("softmax".into()),
                ..Default::default()
            },
        );
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "activate");

        // Nonexistent body text matches nothing
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                body_pattern: Some("nonexistent".into()),
                ..Default::default()
            },
        );
        assert!(result.nodes.is_empty());

        // Nodes with body: None are excluded by body_pattern filter
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                body_pattern: Some("fuse".into()),
                ..Default::default()
            },
        );
        // "fuse" appears in names/docstrings but no node has it in body text
        assert!(result.nodes.is_empty());
    }

    #[test]
    fn test_search_by_file_prefix() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                file_prefix: Some("brain/store".into()),
                ..Default::default()
            },
        );
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "promote");
    }

    #[test]
    fn test_search_combined() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                kind: Some(CodeNodeKind::Function),
                min_complexity: Some(5),
                ..Default::default()
            },
        );
        assert_eq!(result.nodes.len(), 1); // only fuse has complexity >= 5
        assert_eq!(result.nodes[0].name, "fuse");
    }

    #[test]
    fn test_search_with_limit() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = search_nodes(
            &nodes,
            &CodeSearch {
                limit: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(result.nodes.len(), 2);
    }

    #[test]
    fn test_search_empty_batch() {
        let nodes = build_code_nodes_batch(&[]).unwrap();
        let result = search_nodes(&nodes, &CodeSearch::default());
        assert_eq!(result.nodes.len(), 0);
        assert_eq!(result.total_scanned, 0);
    }

    // ── Edge-based search tests ─────────────────────────────────────────

    #[test]
    fn test_callers_of_validate() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = callers("func:brain/signal.py::validate", &nodes, &edges);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "fuse");
    }

    #[test]
    fn test_callees_of_fuse() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = callees("func:brain/signal.py::fuse", &nodes, &edges);
        assert_eq!(result.len(), 2); // validate + promote
        let names: HashSet<&str> = result.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains("validate"));
        assert!(names.contains("promote"));
    }

    #[test]
    fn test_tests_for_fuse() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = tests_for("func:brain/signal.py::fuse", &nodes, &edges);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "test_fuse");
    }

    #[test]
    fn test_children_of_module() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = children_of("mod:brain/signal.py", &nodes, &edges);
        assert_eq!(result.len(), 2); // fuse + validate
    }

    #[test]
    fn test_callers_of_promote() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = callers("func:brain/store.py::promote", &nodes, &edges);
        assert_eq!(result.len(), 2); // fuse + validate
    }

    #[test]
    fn test_no_callers() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = callers("func:brain/signal.py::fuse", &nodes, &edges);
        // fuse has no callers (only tested, not called)
        assert!(result.is_empty());
    }

    // ── Transitive search tests ─────────────────────────────────────────

    #[test]
    fn test_transitive_callees_depth_1() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = transitive_deps(
            "func:brain/signal.py::fuse",
            CodeEdgePredicate::Calls,
            1,
            &nodes,
            &edges,
        );
        // Depth 1: fuse→validate, fuse→promote
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_transitive_callees_depth_2() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = transitive_deps(
            "func:brain/signal.py::fuse",
            CodeEdgePredicate::Calls,
            2,
            &nodes,
            &edges,
        );
        // Depth 2: fuse→validate, fuse→promote, validate→promote (already visited)
        assert_eq!(result.len(), 2); // validate + promote (promote not double-counted)
    }

    #[test]
    fn test_transitive_callers() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = transitive_callers(
            "func:brain/store.py::promote",
            CodeEdgePredicate::Calls,
            2,
            &nodes,
            &edges,
        );
        // promote is called by fuse and validate, fuse is called by nobody
        assert_eq!(result.len(), 2); // fuse + validate
    }

    #[test]
    fn test_transitive_depth_0_returns_nothing() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = transitive_deps(
            "func:brain/signal.py::fuse",
            CodeEdgePredicate::Calls,
            0,
            &nodes,
            &edges,
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_sources_empty_edges() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&[]).unwrap();

        let result = find_sources(
            "func:brain/signal.py::fuse",
            CodeEdgePredicate::Calls,
            &nodes,
            &edges,
        );
        assert!(result.is_empty());
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
        assert_eq!(extract_file_path("no-prefix"), None);
    }
}
