//! Graph traversal — generic BFS/DFS over Arrow edge RecordBatches.
//!
//! Parameterized by column indices so the same traversal logic works for:
//! - nusy-kanban: `depends_on` column in items table
//! - nusy-codegraph: `source_id`/`target_id` columns in edges table
//! - Being cognitive graphs: causal chains, learning paths

use arrow::array::{Array, RecordBatch, StringArray};
use std::collections::{HashMap, HashSet, VecDeque};

/// Configuration for edge column layout in a RecordBatch.
///
/// Different consumers have different schemas — this struct lets you
/// specify which columns contain source IDs, target IDs, and predicates.
#[derive(Debug, Clone)]
pub struct EdgeSchema {
    /// Column index for source node ID.
    pub source_col: usize,
    /// Column index for target node ID.
    pub target_col: usize,
    /// Column index for edge predicate/type (optional — None means "all edges").
    pub predicate_col: Option<usize>,
}

/// Direction of traversal relative to a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Follow edges where the node is the source (find targets).
    Forward,
    /// Follow edges where the node is the target (find sources).
    Reverse,
}

/// Build an adjacency list from an edge RecordBatch.
///
/// If `predicate_filter` is provided, only edges with matching predicate
/// are included. Direction determines how source/target map to from/to.
pub fn build_adjacency(
    edges: &RecordBatch,
    schema: &EdgeSchema,
    direction: Direction,
    predicate_filter: Option<&str>,
) -> HashMap<String, Vec<String>> {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();

    if edges.num_rows() == 0 {
        return adj;
    }

    let Some(sources) = edges
        .column(schema.source_col)
        .as_any()
        .downcast_ref::<StringArray>()
    else {
        return adj;
    };
    let Some(targets) = edges
        .column(schema.target_col)
        .as_any()
        .downcast_ref::<StringArray>()
    else {
        return adj;
    };

    let predicates = schema
        .predicate_col
        .and_then(|col| edges.column(col).as_any().downcast_ref::<StringArray>());

    for i in 0..edges.num_rows() {
        // Filter by predicate if specified
        if let (Some(filter), Some(pred_col)) = (predicate_filter, predicates)
            && (pred_col.is_null(i) || pred_col.value(i) != filter)
        {
            continue;
        }

        if sources.is_null(i) || targets.is_null(i) {
            continue;
        }

        match direction {
            Direction::Forward => {
                adj.entry(sources.value(i).to_string())
                    .or_default()
                    .push(targets.value(i).to_string());
            }
            Direction::Reverse => {
                adj.entry(targets.value(i).to_string())
                    .or_default()
                    .push(sources.value(i).to_string());
            }
        }
    }

    adj
}

/// A traversal result — node ID with its depth from the start.
#[derive(Debug, Clone)]
pub struct TraversalNode {
    pub id: String,
    pub depth: usize,
}

/// BFS traversal from `start_id` following edges up to `max_depth` hops.
///
/// Returns nodes in breadth-first order (excluding the start node).
/// Works with any edge RecordBatch via `EdgeSchema` configuration.
pub fn bfs(
    start_id: &str,
    edges: &RecordBatch,
    schema: &EdgeSchema,
    direction: Direction,
    predicate_filter: Option<&str>,
    max_depth: usize,
) -> Vec<TraversalNode> {
    let adj = build_adjacency(edges, schema, direction, predicate_filter);

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(start_id.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((start_id.to_string(), 0));
    let mut result: Vec<TraversalNode> = Vec::new();

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(neighbors) = adj.get(&current) {
            for neighbor in neighbors {
                if visited.insert(neighbor.clone()) {
                    result.push(TraversalNode {
                        id: neighbor.clone(),
                        depth: depth + 1,
                    });
                    queue.push_back((neighbor.clone(), depth + 1));
                }
            }
        }
    }

    result
}

/// Build an adjacency list from a simple string list column.
///
/// This variant handles the kanban `depends_on` pattern where dependencies
/// are stored as a List<Utf8> column on each item, rather than in a
/// separate edges table.
pub fn build_adjacency_from_list(
    batch: &RecordBatch,
    id_col: usize,
    list_col: usize,
    direction: Direction,
) -> HashMap<String, Vec<String>> {
    use arrow::array::ListArray;

    let mut adj: HashMap<String, Vec<String>> = HashMap::new();

    if batch.num_rows() == 0 {
        return adj;
    }

    let Some(ids) = batch.column(id_col).as_any().downcast_ref::<StringArray>() else {
        return adj;
    };
    let Some(lists) = batch.column(list_col).as_any().downcast_ref::<ListArray>() else {
        return adj;
    };

    for i in 0..batch.num_rows() {
        if lists.is_null(i) {
            continue;
        }
        let values = lists.value(i);
        let Some(str_arr) = values.as_any().downcast_ref::<StringArray>() else {
            continue;
        };

        let id = ids.value(i);
        for j in 0..str_arr.len() {
            if str_arr.is_null(j) {
                continue;
            }
            let dep = str_arr.value(j);
            match direction {
                Direction::Forward => {
                    adj.entry(id.to_string()).or_default().push(dep.to_string());
                }
                Direction::Reverse => {
                    adj.entry(dep.to_string()).or_default().push(id.to_string());
                }
            }
        }
    }

    adj
}

/// BFS traversal using a pre-built adjacency list.
///
/// Useful when the adjacency list comes from `build_adjacency_from_list`
/// or has been cached.
pub fn bfs_with_adjacency(
    start_id: &str,
    adj: &HashMap<String, Vec<String>>,
    max_depth: usize,
) -> Vec<TraversalNode> {
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(start_id.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((start_id.to_string(), 0));
    let mut result: Vec<TraversalNode> = Vec::new();

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(neighbors) = adj.get(&current) {
            for neighbor in neighbors {
                if visited.insert(neighbor.clone()) {
                    result.push(TraversalNode {
                        id: neighbor.clone(),
                        depth: depth + 1,
                    });
                    queue.push_back((neighbor.clone(), depth + 1));
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ListBuilder, StringBuilder};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    /// Build a simple edges RecordBatch: source, target, predicate.
    fn make_edges(triples: &[(&str, &str, &str)]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("source", DataType::Utf8, false),
            Field::new("target", DataType::Utf8, false),
            Field::new("predicate", DataType::Utf8, false),
        ]));
        let sources: Vec<&str> = triples.iter().map(|(s, _, _)| *s).collect();
        let targets: Vec<&str> = triples.iter().map(|(_, t, _)| *t).collect();
        let preds: Vec<&str> = triples.iter().map(|(_, _, p)| *p).collect();

        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(sources)),
                Arc::new(StringArray::from(targets)),
                Arc::new(StringArray::from(preds)),
            ],
        )
        .expect("build edges batch")
    }

    fn edge_schema() -> EdgeSchema {
        EdgeSchema {
            source_col: 0,
            target_col: 1,
            predicate_col: Some(2),
        }
    }

    #[test]
    fn test_build_adjacency_forward() {
        let edges = make_edges(&[
            ("A", "B", "calls"),
            ("A", "C", "calls"),
            ("B", "C", "calls"),
        ]);
        let adj = build_adjacency(&edges, &edge_schema(), Direction::Forward, Some("calls"));

        assert_eq!(adj.get("A").unwrap().len(), 2);
        assert_eq!(adj.get("B").unwrap().len(), 1);
        assert!(adj.get("C").is_none());
    }

    #[test]
    fn test_build_adjacency_reverse() {
        let edges = make_edges(&[
            ("A", "B", "calls"),
            ("A", "C", "calls"),
            ("B", "C", "calls"),
        ]);
        let adj = build_adjacency(&edges, &edge_schema(), Direction::Reverse, Some("calls"));

        assert!(adj.get("A").is_none()); // nothing calls A
        assert_eq!(adj.get("B").unwrap(), &["A"]);
        assert_eq!(adj.get("C").unwrap().len(), 2); // A and B call C
    }

    #[test]
    fn test_build_adjacency_predicate_filter() {
        let edges = make_edges(&[
            ("A", "B", "calls"),
            ("A", "C", "tests"),
            ("B", "C", "calls"),
        ]);
        let adj = build_adjacency(&edges, &edge_schema(), Direction::Forward, Some("calls"));

        assert_eq!(adj.get("A").unwrap(), &["B"]); // only "calls" edges
    }

    #[test]
    fn test_build_adjacency_no_filter() {
        let edges = make_edges(&[
            ("A", "B", "calls"),
            ("A", "C", "tests"),
            ("B", "C", "calls"),
        ]);
        let adj = build_adjacency(&edges, &edge_schema(), Direction::Forward, None);

        assert_eq!(adj.get("A").unwrap().len(), 2); // both edges
    }

    #[test]
    fn test_bfs_depth_1() {
        let edges = make_edges(&[("A", "B", "dep"), ("A", "C", "dep"), ("B", "D", "dep")]);
        let result = bfs(
            "A",
            &edges,
            &edge_schema(),
            Direction::Forward,
            Some("dep"),
            1,
        );

        assert_eq!(result.len(), 2); // B and C
        assert!(result.iter().all(|n| n.depth == 1));
    }

    #[test]
    fn test_bfs_depth_2() {
        let edges = make_edges(&[("A", "B", "dep"), ("B", "C", "dep"), ("C", "D", "dep")]);
        let result = bfs(
            "A",
            &edges,
            &edge_schema(),
            Direction::Forward,
            Some("dep"),
            2,
        );

        assert_eq!(result.len(), 2); // B (depth 1) and C (depth 2)
        assert_eq!(result[0].id, "B");
        assert_eq!(result[0].depth, 1);
        assert_eq!(result[1].id, "C");
        assert_eq!(result[1].depth, 2);
    }

    #[test]
    fn test_bfs_depth_0() {
        let edges = make_edges(&[("A", "B", "dep")]);
        let result = bfs(
            "A",
            &edges,
            &edge_schema(),
            Direction::Forward,
            Some("dep"),
            0,
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_bfs_reverse() {
        let edges = make_edges(&[("A", "C", "dep"), ("B", "C", "dep")]);
        let result = bfs(
            "C",
            &edges,
            &edge_schema(),
            Direction::Reverse,
            Some("dep"),
            1,
        );

        assert_eq!(result.len(), 2); // A and B
    }

    #[test]
    fn test_bfs_cycle_safe() {
        let edges = make_edges(&[("A", "B", "dep"), ("B", "A", "dep")]);
        let result = bfs(
            "A",
            &edges,
            &edge_schema(),
            Direction::Forward,
            Some("dep"),
            10,
        );

        assert_eq!(result.len(), 1); // only B (A is already visited)
    }

    #[test]
    fn test_bfs_empty_edges() {
        let edges = make_edges(&[]);
        let result = bfs("A", &edges, &edge_schema(), Direction::Forward, None, 5);
        assert!(result.is_empty());
    }

    #[test]
    fn test_build_adjacency_from_list() {
        // Build a batch with id + depends_on (list column)
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "depends_on",
                DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
                false,
            ),
        ]));

        let ids = StringArray::from(vec!["A", "B", "C"]);
        let mut list_builder = ListBuilder::new(StringBuilder::new());
        // A depends on B and C
        list_builder.values().append_value("B");
        list_builder.values().append_value("C");
        list_builder.append(true);
        // B depends on C
        list_builder.values().append_value("C");
        list_builder.append(true);
        // C depends on nothing
        list_builder.append(true);

        let batch =
            RecordBatch::try_new(schema, vec![Arc::new(ids), Arc::new(list_builder.finish())])
                .unwrap();

        let adj = build_adjacency_from_list(&batch, 0, 1, Direction::Forward);
        assert_eq!(adj.get("A").unwrap().len(), 2);
        assert_eq!(adj.get("B").unwrap().len(), 1);
        assert!(adj.get("C").is_none()); // C has no deps

        // Reverse: "who depends on X?"
        let adj_rev = build_adjacency_from_list(&batch, 0, 1, Direction::Reverse);
        assert!(adj_rev.get("A").is_none()); // nobody depends on A
        assert_eq!(adj_rev.get("B").unwrap(), &["A"]);
        assert_eq!(adj_rev.get("C").unwrap().len(), 2); // A and B depend on C
    }

    #[test]
    fn test_bfs_with_adjacency() {
        let mut adj = HashMap::new();
        adj.insert("A".to_string(), vec!["B".to_string(), "C".to_string()]);
        adj.insert("B".to_string(), vec!["D".to_string()]);

        let result = bfs_with_adjacency("A", &adj, 2);
        assert_eq!(result.len(), 3); // B, C, D
    }
}
