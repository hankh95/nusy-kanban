//! MCP tool implementations for CodeGraph query and update operations.
//!
//! Provides 4 CRUD-style tools that agents use instead of file reads/writes:
//! - `codegraph_query_objects` — filter by kind, name, parent, metrics, semantic similarity
//! - `codegraph_update_object` — modify CodeNode fields (signature, docstring, body_hash)
//! - `codegraph_add_edge` — create a relationship between CodeNodes
//! - `codegraph_remove_edge` — logical delete of an edge
//!
//! These operate directly on Arrow RecordBatches — no file materialization.

use crate::schema::{
    CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind, build_code_edges_batch, edge_col, node_col,
};
use arrow::array::{Array, Float64Array, Int32Array, RecordBatch, StringArray};

/// Errors from MCP tool operations.
#[derive(Debug, thiserror::Error)]
pub enum McpToolError {
    #[error("Node not found: {0}")]
    NodeNotFound(String),

    #[error("Edge not found: {0} -> {1} ({2})")]
    EdgeNotFound(String, String, String),

    #[error("Invalid kind: {0}")]
    InvalidKind(String),

    #[error("Invalid predicate: {0}")]
    InvalidPredicate(String),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

pub type Result<T> = std::result::Result<T, McpToolError>;

/// Filter criteria for querying code objects.
#[derive(Debug, Default, Clone)]
pub struct QueryFilter {
    /// Filter by CodeNodeKind (e.g., "function", "class").
    pub kind: Option<String>,
    /// Filter by name substring match.
    pub name_contains: Option<String>,
    /// Filter by parent_id exact match.
    pub parent_id: Option<String>,
    /// Filter by minimum LOC.
    pub min_loc: Option<i32>,
    /// Filter by minimum cyclomatic complexity.
    pub min_complexity: Option<i32>,
    /// Filter by maximum coverage percentage.
    pub max_coverage: Option<f64>,
    /// Limit number of results.
    pub limit: Option<usize>,
}

/// Result of a query — matching CodeNodes extracted from RecordBatches.
#[derive(Debug)]
pub struct QueryResult {
    pub nodes: Vec<CodeNode>,
    pub total_scanned: usize,
    pub total_matched: usize,
}

/// Query code objects from the nodes batch with optional filters.
pub fn codegraph_query_objects(
    nodes_batch: &RecordBatch,
    filter: &QueryFilter,
) -> Result<QueryResult> {
    let ids = nodes_batch
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column should be StringArray");
    let names = nodes_batch
        .column(node_col::NAME)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("name column");
    let parent_ids = nodes_batch
        .column(node_col::PARENT_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("parent_id column");
    let signatures = nodes_batch
        .column(node_col::SIGNATURE)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("signature column");
    let docstrings = nodes_batch
        .column(node_col::DOCSTRING)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("docstring column");
    let body_hashes = nodes_batch
        .column(node_col::BODY_HASH)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("body_hash column");
    let locs = nodes_batch
        .column(node_col::LOC)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("loc column");
    let complexities = nodes_batch
        .column(node_col::CYCLOMATIC_COMPLEXITY)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("complexity column");
    let coverages = nodes_batch
        .column(node_col::COVERAGE_PCT)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("coverage column");

    // Extract kind values from dictionary-encoded column
    let kind_col = nodes_batch.column(node_col::KIND);
    let kind_dict = kind_col
        .as_any()
        .downcast_ref::<arrow::array::Int8DictionaryArray>()
        .expect("kind should be dictionary");
    let kind_values = kind_dict
        .values()
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("kind values");

    let total_scanned = nodes_batch.num_rows();
    let mut matched = Vec::new();

    for i in 0..total_scanned {
        // Kind filter
        let kind_key = kind_dict.keys().value(i) as usize;
        let kind_str = kind_values.value(kind_key);
        if let Some(ref filter_kind) = filter.kind
            && kind_str != filter_kind.as_str()
        {
            continue;
        }

        // Name filter (substring match)
        if let Some(ref name_substr) = filter.name_contains {
            let name = names.value(i);
            if !name.to_lowercase().contains(&name_substr.to_lowercase()) {
                continue;
            }
        }

        // Parent filter
        if let Some(ref parent) = filter.parent_id
            && (parent_ids.is_null(i) || parent_ids.value(i) != parent.as_str())
        {
            continue;
        }

        // LOC filter
        if let Some(min_loc) = filter.min_loc
            && (locs.is_null(i) || locs.value(i) < min_loc)
        {
            continue;
        }

        // Complexity filter
        if let Some(min_complexity) = filter.min_complexity
            && (complexities.is_null(i) || complexities.value(i) < min_complexity)
        {
            continue;
        }

        // Coverage filter (max coverage = find under-tested code)
        if let Some(max_cov) = filter.max_coverage
            && (coverages.is_null(i) || coverages.value(i) > max_cov)
        {
            continue;
        }

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
            body: None, // Body not extracted in query results
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

        matched.push(node);

        // Limit
        if let Some(limit) = filter.limit
            && matched.len() >= limit
        {
            break;
        }
    }

    let total_matched = matched.len();
    Ok(QueryResult {
        nodes: matched,
        total_scanned,
        total_matched,
    })
}

/// Fields that can be updated on a CodeNode.
#[derive(Debug, Default, Clone)]
pub struct NodeUpdate {
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub body_hash: Option<String>,
    pub body: Option<String>,
    pub loc: Option<i32>,
    pub cyclomatic_complexity: Option<i32>,
    pub coverage_pct: Option<f64>,
}

/// Update a CodeNode's fields in-place. Returns the updated batch.
///
/// Finds the node by ID, applies the updates, and returns a new RecordBatch
/// with the modifications. The original batch is not mutated.
pub fn codegraph_update_object(
    nodes_batch: &RecordBatch,
    node_id: &str,
    updates: &NodeUpdate,
) -> Result<RecordBatch> {
    let ids = nodes_batch
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column");

    // Find the row
    let row_idx = (0..nodes_batch.num_rows())
        .find(|&i| ids.value(i) == node_id)
        .ok_or_else(|| McpToolError::NodeNotFound(node_id.to_string()))?;

    // Rebuild columns with updates applied
    let mut columns: Vec<arrow::array::ArrayRef> = Vec::new();
    for col_idx in 0..nodes_batch.num_columns() {
        match col_idx {
            node_col::SIGNATURE if updates.signature.is_some() => {
                let old = nodes_batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("signature");
                let mut vals: Vec<Option<String>> = (0..nodes_batch.num_rows())
                    .map(|i| {
                        if old.is_null(i) {
                            None
                        } else {
                            Some(old.value(i).to_string())
                        }
                    })
                    .collect();
                vals[row_idx] = updates.signature.clone();
                let refs: Vec<Option<&str>> = vals.iter().map(|s| s.as_deref()).collect();
                columns.push(std::sync::Arc::new(StringArray::from(refs)));
            }
            node_col::DOCSTRING if updates.docstring.is_some() => {
                let old = nodes_batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("docstring");
                let mut vals: Vec<Option<String>> = (0..nodes_batch.num_rows())
                    .map(|i| {
                        if old.is_null(i) {
                            None
                        } else {
                            Some(old.value(i).to_string())
                        }
                    })
                    .collect();
                vals[row_idx] = updates.docstring.clone();
                let refs: Vec<Option<&str>> = vals.iter().map(|s| s.as_deref()).collect();
                columns.push(std::sync::Arc::new(StringArray::from(refs)));
            }
            // When body is updated, body_hash is auto-recomputed (explicit body_hash ignored).
            node_col::BODY_HASH if updates.body_hash.is_some() || updates.body.is_some() => {
                let old = nodes_batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("body_hash");
                let mut vals: Vec<Option<String>> = (0..nodes_batch.num_rows())
                    .map(|i| {
                        if old.is_null(i) {
                            None
                        } else {
                            Some(old.value(i).to_string())
                        }
                    })
                    .collect();
                // If body is updated, auto-recompute body_hash
                if let Some(ref body) = updates.body {
                    vals[row_idx] = Some(crate::parser::sha256_hex(body.as_bytes()));
                } else {
                    vals[row_idx] = updates.body_hash.clone();
                }
                let refs: Vec<Option<&str>> = vals.iter().map(|s| s.as_deref()).collect();
                columns.push(std::sync::Arc::new(StringArray::from(refs)));
            }
            node_col::BODY if updates.body.is_some() => {
                let old = nodes_batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<arrow::array::LargeStringArray>()
                    .expect("body");
                let mut vals: Vec<Option<String>> = (0..nodes_batch.num_rows())
                    .map(|i| {
                        if old.is_null(i) {
                            None
                        } else {
                            Some(old.value(i).to_string())
                        }
                    })
                    .collect();
                vals[row_idx] = updates.body.clone();
                let refs: Vec<Option<&str>> = vals.iter().map(|s| s.as_deref()).collect();
                columns.push(std::sync::Arc::new(arrow::array::LargeStringArray::from(
                    refs,
                )));
            }
            node_col::LOC if updates.loc.is_some() => {
                let old = nodes_batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<Int32Array>()
                    .expect("loc");
                let mut vals: Vec<Option<i32>> = (0..nodes_batch.num_rows())
                    .map(|i| {
                        if old.is_null(i) {
                            None
                        } else {
                            Some(old.value(i))
                        }
                    })
                    .collect();
                vals[row_idx] = updates.loc;
                columns.push(std::sync::Arc::new(Int32Array::from(vals)));
            }
            node_col::CYCLOMATIC_COMPLEXITY if updates.cyclomatic_complexity.is_some() => {
                let old = nodes_batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<Int32Array>()
                    .expect("complexity");
                let mut vals: Vec<Option<i32>> = (0..nodes_batch.num_rows())
                    .map(|i| {
                        if old.is_null(i) {
                            None
                        } else {
                            Some(old.value(i))
                        }
                    })
                    .collect();
                vals[row_idx] = updates.cyclomatic_complexity;
                columns.push(std::sync::Arc::new(Int32Array::from(vals)));
            }
            node_col::COVERAGE_PCT if updates.coverage_pct.is_some() => {
                let old = nodes_batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<Float64Array>()
                    .expect("coverage");
                let mut vals: Vec<Option<f64>> = (0..nodes_batch.num_rows())
                    .map(|i| {
                        if old.is_null(i) {
                            None
                        } else {
                            Some(old.value(i))
                        }
                    })
                    .collect();
                vals[row_idx] = updates.coverage_pct;
                columns.push(std::sync::Arc::new(Float64Array::from(vals)));
            }
            _ => {
                columns.push(nodes_batch.column(col_idx).clone());
            }
        }
    }

    let schema = std::sync::Arc::new(nodes_batch.schema().as_ref().clone());
    Ok(RecordBatch::try_new(schema, columns)?)
}

/// Add a new edge to the edges batch. Returns a new batch with the edge appended.
pub fn codegraph_add_edge(
    edges_batch: &RecordBatch,
    source_id: &str,
    target_id: &str,
    predicate: &str,
    weight: Option<f32>,
) -> Result<RecordBatch> {
    let pred = CodeEdgePredicate::parse(predicate)
        .ok_or_else(|| McpToolError::InvalidPredicate(predicate.to_string()))?;

    let new_edge = CodeEdge {
        source_id: source_id.to_string(),
        target_id: target_id.to_string(),
        predicate: pred,
        weight,
        commit_id: None,
    };

    let new_batch = build_code_edges_batch(&[new_edge])?;

    // Concatenate existing + new
    arrow::compute::concat_batches(&edges_batch.schema(), &[edges_batch.clone(), new_batch])
        .map_err(McpToolError::Arrow)
}

/// Remove an edge (by setting weight to -1 as a tombstone marker).
///
/// Returns a new batch with the edge marked. Since Arrow batches are immutable,
/// we rebuild with the edge's weight set to -1.0 (tombstone convention).
pub fn codegraph_remove_edge(
    edges_batch: &RecordBatch,
    source_id: &str,
    target_id: &str,
    predicate: &str,
) -> Result<RecordBatch> {
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

    // Extract predicate values from dictionary
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

    let row_idx = (0..edges_batch.num_rows())
        .find(|&i| {
            let key = pred_dict.keys().value(i) as usize;
            sources.value(i) == source_id
                && targets.value(i) == target_id
                && pred_values.value(key) == predicate
        })
        .ok_or_else(|| {
            McpToolError::EdgeNotFound(
                source_id.to_string(),
                target_id.to_string(),
                predicate.to_string(),
            )
        })?;

    // Rebuild weight column with tombstone
    let mut columns: Vec<arrow::array::ArrayRef> = Vec::new();
    for col_idx in 0..edges_batch.num_columns() {
        if col_idx == edge_col::WEIGHT {
            let old = edges_batch
                .column(col_idx)
                .as_any()
                .downcast_ref::<arrow::array::Float32Array>()
                .expect("weight");
            let mut vals: Vec<Option<f32>> = (0..edges_batch.num_rows())
                .map(|i| {
                    if old.is_null(i) {
                        None
                    } else {
                        Some(old.value(i))
                    }
                })
                .collect();
            vals[row_idx] = Some(-1.0); // Tombstone
            columns.push(std::sync::Arc::new(arrow::array::Float32Array::from(vals)));
        } else {
            columns.push(edges_batch.column(col_idx).clone());
        }
    }

    let schema = std::sync::Arc::new(edges_batch.schema().as_ref().clone());
    Ok(RecordBatch::try_new(schema, columns)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{CodeEdge, CodeNode, build_code_edges_batch, build_code_nodes_batch};

    fn sample_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "func:brain/signal_fusion.py::fuse".into(),
                kind: CodeNodeKind::Function,
                parent_id: Some("mod:brain/signal_fusion.py".into()),
                name: "fuse".into(),
                signature: Some("def fuse(signals: List) -> Decision".into()),
                docstring: Some("Fuse cognitive signals.".into()),
                body_hash: Some("abc123".into()),
                body: None,
                loc: Some(42),
                cyclomatic_complexity: Some(8),
                coverage_pct: Some(0.85),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "class:brain/store.py::DualStore".into(),
                kind: CodeNodeKind::Class,
                parent_id: Some("mod:brain/store.py".into()),
                name: "DualStore".into(),
                signature: None,
                docstring: Some("Fast/slow dual-store.".into()),
                body_hash: Some("def456".into()),
                body: None,
                loc: Some(200),
                cyclomatic_complexity: Some(15),
                coverage_pct: Some(0.60),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/store.py::promote".into(),
                kind: CodeNodeKind::Function,
                parent_id: Some("class:brain/store.py::DualStore".into()),
                name: "promote".into(),
                signature: Some("def promote(self) -> None".into()),
                docstring: None,
                body_hash: Some("ghi789".into()),
                body: None,
                loc: Some(30),
                cyclomatic_complexity: Some(3),
                coverage_pct: Some(0.95),
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    fn sample_edges() -> Vec<CodeEdge> {
        vec![
            CodeEdge {
                source_id: "func:brain/signal_fusion.py::fuse".into(),
                target_id: "class:brain/store.py::DualStore".into(),
                predicate: CodeEdgePredicate::Uses,
                weight: Some(1.0),
                commit_id: None,
            },
            CodeEdge {
                source_id: "func:brain/store.py::promote".into(),
                target_id: "class:brain/store.py::DualStore".into(),
                predicate: CodeEdgePredicate::Contains,
                weight: None,
                commit_id: None,
            },
        ]
    }

    // --- Phase 1: Query tests ---

    #[test]
    fn test_query_all_objects() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = codegraph_query_objects(&batch, &QueryFilter::default()).unwrap();
        assert_eq!(result.total_scanned, 3);
        assert_eq!(result.total_matched, 3);
        assert_eq!(result.nodes.len(), 3);
    }

    #[test]
    fn test_query_by_kind() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let filter = QueryFilter {
            kind: Some("function".into()),
            ..Default::default()
        };
        let result = codegraph_query_objects(&batch, &filter).unwrap();
        assert_eq!(result.total_matched, 2); // fuse + promote
    }

    #[test]
    fn test_query_by_name() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let filter = QueryFilter {
            name_contains: Some("fuse".into()),
            ..Default::default()
        };
        let result = codegraph_query_objects(&batch, &filter).unwrap();
        assert_eq!(result.total_matched, 1);
        assert_eq!(result.nodes[0].name, "fuse");
    }

    #[test]
    fn test_query_by_parent() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let filter = QueryFilter {
            parent_id: Some("class:brain/store.py::DualStore".into()),
            ..Default::default()
        };
        let result = codegraph_query_objects(&batch, &filter).unwrap();
        assert_eq!(result.total_matched, 1);
        assert_eq!(result.nodes[0].name, "promote");
    }

    #[test]
    fn test_query_by_min_loc() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let filter = QueryFilter {
            min_loc: Some(100),
            ..Default::default()
        };
        let result = codegraph_query_objects(&batch, &filter).unwrap();
        assert_eq!(result.total_matched, 1);
        assert_eq!(result.nodes[0].name, "DualStore");
    }

    #[test]
    fn test_query_by_max_coverage() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let filter = QueryFilter {
            max_coverage: Some(0.70),
            ..Default::default()
        };
        let result = codegraph_query_objects(&batch, &filter).unwrap();
        assert_eq!(result.total_matched, 1);
        assert_eq!(result.nodes[0].name, "DualStore"); // 0.60 coverage
    }

    #[test]
    fn test_query_with_limit() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let filter = QueryFilter {
            limit: Some(2),
            ..Default::default()
        };
        let result = codegraph_query_objects(&batch, &filter).unwrap();
        assert_eq!(result.total_matched, 2);
    }

    #[test]
    fn test_query_combined_filters() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let filter = QueryFilter {
            kind: Some("function".into()),
            min_complexity: Some(5),
            ..Default::default()
        };
        let result = codegraph_query_objects(&batch, &filter).unwrap();
        assert_eq!(result.total_matched, 1); // only fuse has complexity >= 5 AND is function
        assert_eq!(result.nodes[0].name, "fuse");
    }

    #[test]
    fn test_query_empty_batch() {
        let batch = build_code_nodes_batch(&[]).unwrap();
        let result = codegraph_query_objects(&batch, &QueryFilter::default()).unwrap();
        assert_eq!(result.total_matched, 0);
    }

    // --- Phase 1: Update tests ---

    #[test]
    fn test_update_docstring() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let updated = codegraph_update_object(
            &batch,
            "func:brain/signal_fusion.py::fuse",
            &NodeUpdate {
                docstring: Some("Updated docstring.".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // Query the updated batch
        let result = codegraph_query_objects(
            &updated,
            &QueryFilter {
                name_contains: Some("fuse".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.nodes[0].docstring, Some("Updated docstring.".into()));
    }

    #[test]
    fn test_update_multiple_fields() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let updated = codegraph_update_object(
            &batch,
            "func:brain/store.py::promote",
            &NodeUpdate {
                loc: Some(50),
                coverage_pct: Some(0.99),
                ..Default::default()
            },
        )
        .unwrap();

        let result = codegraph_query_objects(
            &updated,
            &QueryFilter {
                name_contains: Some("promote".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.nodes[0].loc, Some(50));
        assert_eq!(result.nodes[0].coverage_pct, Some(0.99));
    }

    #[test]
    fn test_update_nonexistent_node() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = codegraph_update_object(
            &batch,
            "func:nonexistent::foo",
            &NodeUpdate {
                docstring: Some("nope".into()),
                ..Default::default()
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_update_preserves_other_nodes() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let updated = codegraph_update_object(
            &batch,
            "func:brain/signal_fusion.py::fuse",
            &NodeUpdate {
                docstring: Some("Changed.".into()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(updated.num_rows(), 3);
        // Other nodes unchanged
        let result = codegraph_query_objects(
            &updated,
            &QueryFilter {
                name_contains: Some("DualStore".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            result.nodes[0].docstring,
            Some("Fast/slow dual-store.".into())
        );
    }

    // --- Phase 1: Edge tests ---

    #[test]
    fn test_add_edge() {
        let batch = build_code_edges_batch(&sample_edges()).unwrap();
        let updated = codegraph_add_edge(
            &batch,
            "func:brain/signal_fusion.py::fuse",
            "func:brain/store.py::promote",
            "calls",
            Some(1.0),
        )
        .unwrap();

        assert_eq!(updated.num_rows(), 3); // 2 original + 1 new
    }

    #[test]
    fn test_add_edge_invalid_predicate() {
        let batch = build_code_edges_batch(&sample_edges()).unwrap();
        let result = codegraph_add_edge(&batch, "a", "b", "nonexistent_pred", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_edge() {
        let batch = build_code_edges_batch(&sample_edges()).unwrap();
        let updated = codegraph_remove_edge(
            &batch,
            "func:brain/signal_fusion.py::fuse",
            "class:brain/store.py::DualStore",
            "uses",
        )
        .unwrap();

        // Row count unchanged but weight is -1 (tombstone)
        assert_eq!(updated.num_rows(), 2);
        let weights = updated
            .column(edge_col::WEIGHT)
            .as_any()
            .downcast_ref::<arrow::array::Float32Array>()
            .unwrap();
        assert_eq!(weights.value(0), -1.0);
    }

    #[test]
    fn test_remove_edge_not_found() {
        let batch = build_code_edges_batch(&sample_edges()).unwrap();
        let result = codegraph_remove_edge(&batch, "a", "b", "calls");
        assert!(result.is_err());
    }

    #[test]
    fn test_query_update_query_round_trip() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();

        // Query original
        let before = codegraph_query_objects(
            &batch,
            &QueryFilter {
                name_contains: Some("fuse".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            before.nodes[0].docstring,
            Some("Fuse cognitive signals.".into())
        );

        // Update
        let updated = codegraph_update_object(
            &batch,
            "func:brain/signal_fusion.py::fuse",
            &NodeUpdate {
                docstring: Some("Parallel weighted voting.".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // Query again
        let after = codegraph_query_objects(
            &updated,
            &QueryFilter {
                name_contains: Some("fuse".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            after.nodes[0].docstring,
            Some("Parallel weighted voting.".into())
        );
    }
}
