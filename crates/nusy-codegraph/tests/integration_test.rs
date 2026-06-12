//! Integration tests for nusy-codegraph.
//!
//! Tests the full pipeline: parse Python source → build CodeNodes/CodeEdges →
//! Arrow RecordBatches → verify structure and relationships.

use arrow::array::Array;
use nusy_codegraph::{
    CodeEdgePredicate, CodeNodeKind, NameResolver, build_code_edges_batch, build_code_nodes_batch,
    extract_edges, parse_python_file,
};
use std::path::PathBuf;

// ─── Multi-file parsing ─────────────────────────────────────────────────────

const FILE_A: &str = r#"
"""Module A: signal processing."""

from brain.module_b import DataStore

class SignalProcessor:
    """Processes incoming signals."""

    def __init__(self, store: DataStore):
        self.store = store

    def process(self, signal: dict) -> dict:
        """Process a single signal."""
        if signal.get("type") == "critical":
            return self._handle_critical(signal)
        return signal

    def _handle_critical(self, signal: dict) -> dict:
        """Handle critical signals with extra care."""
        signal["priority"] = "high"
        return signal

def standalone_helper(x: int) -> int:
    """Helper outside any class."""
    return x * 2

def test_process():
    """Test the process method."""
    proc = SignalProcessor(None)
    assert proc.process({}) == {}
"#;

const FILE_B: &str = r#"
"""Module B: data storage."""

import json
from pathlib import Path

class DataStore:
    """Stores data persistently."""

    def __init__(self, path: str):
        self.path = path
        self.data = {}

    def save(self, key: str, value):
        """Save a key-value pair."""
        self.data[key] = value

    def load(self, key: str):
        """Load a value by key."""
        return self.data.get(key)

class CachedStore(DataStore):
    """DataStore with caching."""

    def __init__(self, path: str, cache_size: int = 100):
        super().__init__(path)
        self.cache_size = cache_size
        self.cache = {}

    def load(self, key: str):
        """Load with cache."""
        if key in self.cache:
            return self.cache[key]
        val = super().load(key)
        if val is not None:
            self.cache[key] = val
        return val
"#;

#[test]
fn test_multi_file_parse_and_edges() {
    let path_a = PathBuf::from("brain/module_a.py");
    let path_b = PathBuf::from("brain/module_b.py");

    let result_a = parse_python_file(&path_a, FILE_A).expect("parse A");
    let result_b = parse_python_file(&path_b, FILE_B).expect("parse B");

    // Verify node counts
    // File A: file, module, class SignalProcessor, __init__, process, _handle_critical,
    //         standalone_helper, test_process = 8 nodes
    // File B: file, module, class DataStore, __init__, save, load,
    //         class CachedStore, __init__, load = 9 nodes
    let total_nodes = result_a.nodes.len() + result_b.nodes.len();
    assert!(total_nodes >= 15, "Expected >= 15 nodes, got {total_nodes}");

    // Build resolver from all nodes
    let all_nodes: Vec<_> = result_a
        .nodes
        .iter()
        .chain(result_b.nodes.iter())
        .cloned()
        .collect();
    let resolver = NameResolver::from_nodes(&all_nodes);

    // Extract edges
    let edges = extract_edges(&[result_a, result_b], &resolver);

    // Should have containment edges
    let containment: Vec<_> = edges
        .iter()
        .filter(|e| e.predicate == CodeEdgePredicate::Contains)
        .collect();
    assert!(
        containment.len() >= 12,
        "Expected >= 12 containment edges, got {}",
        containment.len()
    );

    // Should have import edges
    let imports: Vec<_> = edges
        .iter()
        .filter(|e| e.predicate == CodeEdgePredicate::Imports)
        .collect();
    assert!(!imports.is_empty(), "Should have import edges");

    // Should have inheritance edge (CachedStore → DataStore)
    let inheritance: Vec<_> = edges
        .iter()
        .filter(|e| e.predicate == CodeEdgePredicate::InheritsFrom)
        .collect();
    assert!(!inheritance.is_empty(), "Should have inheritance edges");

    // Verify CachedStore inherits from DataStore
    let cached_inherits = inheritance
        .iter()
        .find(|e| e.source_id.contains("CachedStore"));
    assert!(
        cached_inherits.is_some(),
        "CachedStore should inherit from DataStore"
    );
}

#[test]
fn test_arrow_batch_roundtrip() {
    let path = PathBuf::from("brain/test_module.py");
    let result = parse_python_file(&path, FILE_A).expect("parse");

    // Build CodeNodes batch
    let batch = build_code_nodes_batch(&result.nodes).expect("build nodes batch");
    assert_eq!(batch.num_rows(), result.nodes.len());
    assert_eq!(batch.num_columns(), 19);

    // Verify we can read back the data
    use arrow::array::StringArray;
    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column");

    // All IDs should be non-empty
    for i in 0..ids.len() {
        assert!(
            !ids.value(i).is_empty(),
            "ID at row {i} should be non-empty"
        );
    }

    // Build edges
    let resolver = NameResolver::from_nodes(&result.nodes);
    let edges = extract_edges(&[result], &resolver);
    let edges_batch = build_code_edges_batch(&edges).expect("build edges batch");
    assert!(edges_batch.num_rows() > 0, "Should have edges");
    assert_eq!(edges_batch.num_columns(), 5);
}

#[test]
fn test_node_kinds_distribution() {
    let path = PathBuf::from("brain/test.py");
    let result = parse_python_file(&path, FILE_A).expect("parse");

    let count_kind =
        |kind: CodeNodeKind| -> usize { result.nodes.iter().filter(|n| n.kind == kind).count() };

    assert_eq!(count_kind(CodeNodeKind::File), 1);
    assert_eq!(count_kind(CodeNodeKind::Module), 1);
    assert_eq!(count_kind(CodeNodeKind::Class), 1); // SignalProcessor
    assert!(count_kind(CodeNodeKind::Method) >= 3); // __init__, process, _handle_critical
    assert!(count_kind(CodeNodeKind::Function) >= 1); // standalone_helper
    assert!(count_kind(CodeNodeKind::Test) >= 1); // test_process
}

#[test]
fn test_containment_hierarchy_integrity() {
    let path = PathBuf::from("brain/test.py");
    let result = parse_python_file(&path, FILE_A).expect("parse");

    // Every node except the file should have a parent_id
    for node in &result.nodes {
        if node.kind != CodeNodeKind::File {
            assert!(
                node.parent_id.is_some(),
                "{} ({}) should have parent_id",
                node.id,
                node.kind
            );
        }
    }

    // Every parent_id should reference an existing node
    let ids: std::collections::HashSet<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
    for node in &result.nodes {
        if let Some(parent_id) = &node.parent_id {
            assert!(
                ids.contains(parent_id.as_str()),
                "{} has parent_id {} which doesn't exist",
                node.id,
                parent_id
            );
        }
    }
}

#[test]
fn test_docstring_extraction_quality() {
    let path = PathBuf::from("test.py");
    let result = parse_python_file(&path, FILE_B).expect("parse");

    // DataStore class should have docstring
    let store = result
        .nodes
        .iter()
        .find(|n| n.name == "DataStore" && n.kind == CodeNodeKind::Class)
        .expect("DataStore should exist");
    assert_eq!(
        store.docstring.as_deref(),
        Some("Stores data persistently.")
    );

    // save method should have docstring
    let save = result
        .nodes
        .iter()
        .find(|n| n.name == "save" && n.kind == CodeNodeKind::Method)
        .expect("save should exist");
    assert_eq!(save.docstring.as_deref(), Some("Save a key-value pair."));
}

#[test]
fn test_body_hash_uniqueness() {
    let path = PathBuf::from("test.py");
    let result = parse_python_file(&path, FILE_A).expect("parse");

    // Functions with different bodies should have different hashes
    let hashes: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                CodeNodeKind::Function | CodeNodeKind::Method | CodeNodeKind::Test
            )
        })
        .filter_map(|n| n.body_hash.as_deref())
        .collect();

    // All hashes should be unique (different function bodies)
    let unique: std::collections::HashSet<&str> = hashes.iter().copied().collect();
    assert_eq!(
        hashes.len(),
        unique.len(),
        "Body hashes should be unique for different functions"
    );
}

#[test]
fn test_large_batch_performance() {
    use nusy_codegraph::{CodeNode, CodeNodeKind};

    // Build 1000 synthetic nodes
    let nodes: Vec<CodeNode> = (0..1000)
        .map(|i| CodeNode {
            id: format!("func:module_{}.py::func_{i}", i / 10),
            kind: CodeNodeKind::Function,
            parent_id: Some(format!("mod:module_{}.py", i / 10)),
            name: format!("func_{i}"),
            signature: Some(format!("def func_{i}(x: int) -> int")),
            docstring: Some(format!("Function {i} docstring.")),
            body_hash: Some(format!("hash_{i}")),
            body: Some(format!("def func_{i}(x): return x + {i}")),
            loc: Some((i % 100 + 5) as i32),
            cyclomatic_complexity: Some((i % 20 + 1) as i32),
            coverage_pct: Some((i % 100) as f64 / 100.0),
            last_modified: None,
            ..Default::default()
        })
        .collect();

    let start = std::time::Instant::now();
    let batch = build_code_nodes_batch(&nodes).expect("build 1K nodes");
    let elapsed = start.elapsed();

    assert_eq!(batch.num_rows(), 1000);
    assert!(
        elapsed.as_millis() < 100,
        "1K node batch build took {}ms — expected <100ms",
        elapsed.as_millis()
    );
}
