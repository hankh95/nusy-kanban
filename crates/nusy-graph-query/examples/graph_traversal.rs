//! Graph traversal example — BFS/DFS over Arrow edge RecordBatches.
//!
//! Run: `cargo run --example graph_traversal`

use arrow::array::{RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use nusy_graph_query::traversal::*;
use std::sync::Arc;

fn main() {
    // Build an edge table: source → target with predicate
    let schema = Arc::new(Schema::new(vec![
        Field::new("source", DataType::Utf8, false),
        Field::new("target", DataType::Utf8, false),
        Field::new("predicate", DataType::Utf8, false),
    ]));

    let edges = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![
                "main", "main", "parse", "parse", "lex",
            ])),
            Arc::new(StringArray::from(vec![
                "parse", "eval", "lex", "ast", "token",
            ])),
            Arc::new(StringArray::from(vec![
                "calls", "calls", "calls", "calls", "calls",
            ])),
        ],
    )
    .expect("build edges");

    let edge_schema = EdgeSchema {
        source_col: 0,
        target_col: 1,
        predicate_col: Some(2),
    };

    // Forward BFS: what does "main" call (transitively)?
    println!("Forward BFS from 'main' (depth 3):");
    let reachable = bfs(
        "main",
        &edges,
        &edge_schema,
        Direction::Forward,
        Some("calls"),
        3,
    );
    for node in &reachable {
        let indent = "  ".repeat(node.depth);
        println!("  {indent}{} (depth {})", node.id, node.depth);
    }

    // Reverse BFS: what calls "token" (transitively)?
    println!("\nReverse BFS from 'token' (depth 3):");
    let callers = bfs(
        "token",
        &edges,
        &edge_schema,
        Direction::Reverse,
        Some("calls"),
        3,
    );
    for node in &callers {
        let indent = "  ".repeat(node.depth);
        println!("  {indent}{} (depth {})", node.id, node.depth);
    }

    // Build full adjacency list
    println!("\nAdjacency list:");
    let adj = build_adjacency(&edges, &edge_schema, Direction::Forward, None);
    for (from, targets) in &adj {
        println!("  {from} → {}", targets.join(", "));
    }
}
