//! Graph-native rename — rename a code object, all edges follow.
//!
//! Renames a CodeNode ID across nodes and edges batches. All references
//! (parent_id, edge source_id, edge target_id) are updated atomically.
//! Returns new batches — originals are not mutated.

use crate::schema::{edge_col, node_col};
use arrow::array::{Array, RecordBatch, StringArray};
use std::sync::Arc;

/// Errors from rename operations.
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    #[error("Node not found: {0}")]
    NodeNotFound(String),

    #[error("Target ID already exists: {0}")]
    TargetExists(String),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

pub type Result<T> = std::result::Result<T, RenameError>;

/// Rename a CodeNode, updating all references in nodes and edges batches.
///
/// Updates:
/// - The node's own `id` field
/// - Any `parent_id` references pointing to the old ID
/// - Any `source_id` or `target_id` in edges pointing to the old ID
///
/// Returns `(new_nodes_batch, new_edges_batch)`.
pub fn rename_node(
    nodes_batch: &RecordBatch,
    edges_batch: &RecordBatch,
    old_id: &str,
    new_id: &str,
) -> Result<(RecordBatch, RecordBatch)> {
    // Verify old_id exists
    let ids = nodes_batch
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column");

    let found = (0..nodes_batch.num_rows()).any(|i| ids.value(i) == old_id);
    if !found {
        return Err(RenameError::NodeNotFound(old_id.to_string()));
    }

    // Verify new_id doesn't already exist
    let conflict = (0..nodes_batch.num_rows()).any(|i| ids.value(i) == new_id);
    if conflict {
        return Err(RenameError::TargetExists(new_id.to_string()));
    }

    // Rebuild nodes batch
    let new_nodes = rename_in_nodes(nodes_batch, old_id, new_id)?;
    let new_edges = rename_in_edges(edges_batch, old_id, new_id)?;

    Ok((new_nodes, new_edges))
}

/// Rename references in the nodes batch (id + parent_id columns).
fn rename_in_nodes(batch: &RecordBatch, old_id: &str, new_id: &str) -> Result<RecordBatch> {
    let mut columns: Vec<arrow::array::ArrayRef> = Vec::with_capacity(batch.num_columns());

    for col_idx in 0..batch.num_columns() {
        match col_idx {
            node_col::ID => {
                let old = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("id");
                let vals: Vec<String> = (0..batch.num_rows())
                    .map(|i| {
                        let v = old.value(i);
                        if v == old_id {
                            new_id.to_string()
                        } else {
                            v.to_string()
                        }
                    })
                    .collect();
                let refs: Vec<&str> = vals.iter().map(|s| s.as_str()).collect();
                columns.push(Arc::new(StringArray::from(refs)));
            }
            node_col::PARENT_ID => {
                let old = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("parent_id");
                let vals: Vec<Option<String>> = (0..batch.num_rows())
                    .map(|i| {
                        if old.is_null(i) {
                            None
                        } else {
                            let v = old.value(i);
                            if v == old_id {
                                Some(new_id.to_string())
                            } else {
                                Some(v.to_string())
                            }
                        }
                    })
                    .collect();
                let refs: Vec<Option<&str>> = vals.iter().map(|s| s.as_deref()).collect();
                columns.push(Arc::new(StringArray::from(refs)));
            }
            _ => {
                columns.push(batch.column(col_idx).clone());
            }
        }
    }

    let schema = Arc::new(batch.schema().as_ref().clone());
    Ok(RecordBatch::try_new(schema, columns)?)
}

/// Rename references in the edges batch (source_id + target_id columns).
fn rename_in_edges(batch: &RecordBatch, old_id: &str, new_id: &str) -> Result<RecordBatch> {
    if batch.num_rows() == 0 {
        return Ok(batch.clone());
    }

    let mut columns: Vec<arrow::array::ArrayRef> = Vec::with_capacity(batch.num_columns());

    for col_idx in 0..batch.num_columns() {
        match col_idx {
            edge_col::SOURCE_ID | edge_col::TARGET_ID => {
                let old = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("edge string column");
                let vals: Vec<String> = (0..batch.num_rows())
                    .map(|i| {
                        let v = old.value(i);
                        if v == old_id {
                            new_id.to_string()
                        } else {
                            v.to_string()
                        }
                    })
                    .collect();
                let refs: Vec<&str> = vals.iter().map(|s| s.as_str()).collect();
                columns.push(Arc::new(StringArray::from(refs)));
            }
            _ => {
                columns.push(batch.column(col_idx).clone());
            }
        }
    }

    let schema = Arc::new(batch.schema().as_ref().clone());
    Ok(RecordBatch::try_new(schema, columns)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_tools::{QueryFilter, codegraph_query_objects};
    use crate::schema::{
        CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind, build_code_edges_batch,
        build_code_nodes_batch,
    };

    fn sample_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "class:brain/store.py::DualStore".into(),
                kind: CodeNodeKind::Class,
                parent_id: Some("mod:brain/store.py".into()),
                name: "DualStore".into(),
                signature: None,
                docstring: Some("Dual store.".into()),
                body_hash: Some("ds_h".into()),
                body: None,
                loc: Some(100),
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/store.py::DualStore::promote".into(),
                kind: CodeNodeKind::Method,
                parent_id: Some("class:brain/store.py::DualStore".into()),
                name: "promote".into(),
                signature: Some("def promote(self)".into()),
                docstring: None,
                body_hash: Some("prom_h".into()),
                body: None,
                loc: Some(20),
                cyclomatic_complexity: Some(3),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/signal.py::fuse".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "fuse".into(),
                signature: Some("def fuse(signals)".into()),
                docstring: None,
                body_hash: Some("fuse_h".into()),
                body: None,
                loc: Some(30),
                cyclomatic_complexity: Some(5),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    fn sample_edges() -> Vec<CodeEdge> {
        vec![
            CodeEdge {
                source_id: "func:brain/signal.py::fuse".into(),
                target_id: "func:brain/store.py::DualStore::promote".into(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(1.0),
                commit_id: None,
            },
            CodeEdge {
                source_id: "class:brain/store.py::DualStore".into(),
                target_id: "func:brain/store.py::DualStore::promote".into(),
                predicate: CodeEdgePredicate::Contains,
                weight: None,
                commit_id: None,
            },
        ]
    }

    #[test]
    fn test_rename_function() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let (new_nodes, new_edges) = rename_node(
            &nodes,
            &edges,
            "func:brain/signal.py::fuse",
            "func:brain/signal.py::fuse_signals",
        )
        .unwrap();

        // Old ID gone, new ID exists
        let old_result = codegraph_query_objects(
            &new_nodes,
            &QueryFilter {
                name_contains: Some("fuse".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(old_result.nodes.len(), 1);
        assert_eq!(old_result.nodes[0].id, "func:brain/signal.py::fuse_signals");

        // Edge source updated
        let sources = new_edges
            .column(edge_col::SOURCE_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(sources.value(0), "func:brain/signal.py::fuse_signals");
    }

    #[test]
    fn test_rename_class_updates_child_parent() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let (new_nodes, _) = rename_node(
            &nodes,
            &edges,
            "class:brain/store.py::DualStore",
            "class:brain/store.py::GraphStore",
        )
        .unwrap();

        // Child's parent_id should be updated
        let parent_ids = new_nodes
            .column(node_col::PARENT_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        // promote is row 1, parent_id should now be GraphStore
        assert_eq!(parent_ids.value(1), "class:brain/store.py::GraphStore");
    }

    #[test]
    fn test_rename_updates_edge_target() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let (_, new_edges) = rename_node(
            &nodes,
            &edges,
            "func:brain/store.py::DualStore::promote",
            "func:brain/store.py::DualStore::elevate",
        )
        .unwrap();

        // Both edges target promote — should be updated
        let targets = new_edges
            .column(edge_col::TARGET_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(targets.value(0), "func:brain/store.py::DualStore::elevate");
        assert_eq!(targets.value(1), "func:brain/store.py::DualStore::elevate");
    }

    #[test]
    fn test_rename_preserves_unaffected_nodes() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let (new_nodes, _) = rename_node(
            &nodes,
            &edges,
            "func:brain/signal.py::fuse",
            "func:brain/signal.py::fuse2",
        )
        .unwrap();

        assert_eq!(new_nodes.num_rows(), 3);
        let result = codegraph_query_objects(
            &new_nodes,
            &QueryFilter {
                name_contains: Some("DualStore".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.nodes[0].docstring, Some("Dual store.".into()));
    }

    #[test]
    fn test_rename_nonexistent_node_errors() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = rename_node(&nodes, &edges, "func:nonexistent::foo", "func:new::foo");
        assert!(result.is_err());
    }

    #[test]
    fn test_rename_to_existing_id_errors() {
        let nodes = build_code_nodes_batch(&sample_nodes()).unwrap();
        let edges = build_code_edges_batch(&sample_edges()).unwrap();

        let result = rename_node(
            &nodes,
            &edges,
            "func:brain/signal.py::fuse",
            "class:brain/store.py::DualStore", // already exists
        );
        assert!(result.is_err());
    }
}
