//! Integration tests for the full ingestion + embedding + metrics pipeline.
//!
//! Tests the pipeline: directory walk → parse → edges → embeddings → metrics → Arrow batches.

use nusy_codegraph::{
    CodeEdgePredicate, CodeNodeKind, HashEmbeddingProvider, attach_embeddings, callers_of,
    compute_codebase_metrics, embed_nodes, enrich_with_coverage, high_complexity_nodes,
    ingest_directory, largest_nodes, low_coverage_nodes, nodes_in_file, parse_coverage_json,
    semantic_search,
};
use std::collections::HashMap;

/// Build a realistic multi-file Python project for testing.
fn create_test_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temp dir");

    // Create directory structure
    std::fs::create_dir_all(dir.path().join("brain/perception")).expect("mkdir");
    std::fs::create_dir_all(dir.path().join("brain/training")).expect("mkdir");
    std::fs::create_dir_all(dir.path().join("brain/utils")).expect("mkdir");

    // brain/__init__.py
    std::fs::write(
        dir.path().join("brain/__init__.py"),
        r#"
"""NuSy Brain — Core reasoning engine."""

__version__ = "0.14.0"
"#,
    )
    .expect("write");

    // brain/perception/__init__.py
    std::fs::write(dir.path().join("brain/perception/__init__.py"), "").expect("write");

    // brain/perception/signal_fusion.py
    std::fs::write(
        dir.path().join("brain/perception/signal_fusion.py"),
        r#"
"""Signal fusion module — merge multiple signal sources."""

from brain.utils.helpers import normalize

class SignalFusion:
    """Fuses signals from multiple perception sources."""

    def __init__(self, config: dict):
        """Initialize with configuration."""
        self.config = config
        self.weights = config.get("weights", {})

    def fuse(self, signals: list) -> dict:
        """Fuse all signals into a unified representation.

        Applies weighted voting across signal sources.
        """
        result = {}
        for signal in signals:
            if signal.get("type") == "critical":
                result[signal["name"]] = signal["value"]
            elif signal.get("weight", 0) > 0.5:
                result[signal["name"]] = signal["value"]
            elif signal.get("fallback"):
                result[signal["name"]] = signal.get("default", None)
        return result

    def _validate_signal(self, signal: dict) -> bool:
        """Validate a signal before fusion."""
        return "name" in signal and "value" in signal

class MultiModalFusion(SignalFusion):
    """Signal fusion with multi-modal support."""

    def fuse(self, signals: list) -> dict:
        """Override fuse with multi-modal logic."""
        return super().fuse(signals)
"#,
    )
    .expect("write");

    // brain/training/lora_trainer.py
    std::fs::write(
        dir.path().join("brain/training/lora_trainer.py"),
        r#"
"""LoRA training pipeline."""

import json

class LoRATrainer:
    """Trains LoRA adapters for domain specialization."""

    def __init__(self, model_name: str, rank: int = 16):
        """Initialize trainer."""
        self.model_name = model_name
        self.rank = rank
        self.losses = []

    def train(self, dataset, epochs: int = 10) -> dict:
        """Train the LoRA adapter.

        Returns training metrics.
        """
        for epoch in range(epochs):
            loss = self._train_epoch(dataset, epoch)
            self.losses.append(loss)
            if loss < 0.01:
                break
        return {"final_loss": self.losses[-1], "epochs": len(self.losses)}

    def _train_epoch(self, dataset, epoch: int) -> float:
        """Train a single epoch."""
        return 1.0 / (epoch + 1)

    def save(self, path: str):
        """Save the adapter weights."""
        with open(path, "w") as f:
            json.dump({"rank": self.rank, "losses": self.losses}, f)
"#,
    )
    .expect("write");

    // brain/utils/__init__.py
    std::fs::write(dir.path().join("brain/utils/__init__.py"), "").expect("write");

    // brain/utils/helpers.py
    std::fs::write(
        dir.path().join("brain/utils/helpers.py"),
        r#"
"""Utility helpers."""

def normalize(data: list) -> list:
    """Normalize data to [0, 1] range."""
    if not data:
        return []
    min_val = min(data)
    max_val = max(data)
    if max_val == min_val:
        return [0.5] * len(data)
    return [(x - min_val) / (max_val - min_val) for x in data]

def clamp(value: float, low: float = 0.0, high: float = 1.0) -> float:
    """Clamp a value to a range."""
    return max(low, min(high, value))
"#,
    )
    .expect("write");

    // brain/tests/test_signal_fusion.py
    std::fs::create_dir_all(dir.path().join("brain/tests")).expect("mkdir");
    std::fs::write(
        dir.path().join("brain/tests/test_signal_fusion.py"),
        r#"
"""Tests for signal fusion."""

from brain.perception.signal_fusion import SignalFusion

def test_fuse_empty():
    """Test fusing empty signals."""
    sf = SignalFusion({})
    assert sf.fuse([]) == {}

def test_fuse_critical():
    """Test critical signal handling."""
    sf = SignalFusion({})
    result = sf.fuse([{"name": "a", "value": 1, "type": "critical"}])
    assert result == {"a": 1}

def test_validate_signal():
    """Test signal validation."""
    sf = SignalFusion({})
    assert sf._validate_signal({"name": "x", "value": 1})
    assert not sf._validate_signal({"name": "x"})
"#,
    )
    .expect("write");

    dir
}

#[test]
fn test_full_ingestion_pipeline() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");

    let result = ingest_directory(&brain_dir).expect("ingest should succeed");

    // Check node counts
    let file_count = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::File)
        .count();
    assert!(file_count >= 5, "Expected >= 5 files, got {}", file_count);

    let class_count = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::Class)
        .count();
    assert!(
        class_count >= 3,
        "Expected >= 3 classes, got {}",
        class_count
    );

    let func_count = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::Function)
        .count();
    assert!(
        func_count >= 2,
        "Expected >= 2 functions, got {}",
        func_count
    );

    let method_count = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::Method)
        .count();
    assert!(
        method_count >= 5,
        "Expected >= 5 methods, got {}",
        method_count
    );

    // Check edge counts
    let containment_count = result
        .edges
        .iter()
        .filter(|e| e.predicate == CodeEdgePredicate::Contains)
        .count();
    assert!(
        containment_count >= 10,
        "Expected >= 10 containment edges, got {}",
        containment_count
    );

    // Check no parse errors
    assert!(
        result.errors.is_empty(),
        "Should have no parse errors: {:?}",
        result.errors
    );

    // Check summary is well-formed
    let summary = result.summary();
    assert!(summary.contains("Total nodes:"));
    assert!(summary.contains("contains:"));
}

#[test]
fn test_query_functions_in_file() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let result = ingest_directory(&brain_dir).expect("ingest");

    // Query functions in signal_fusion.py
    let signal_nodes = nodes_in_file(&result.nodes, "perception/signal_fusion.py");
    assert!(
        signal_nodes.len() >= 4,
        "Expected >= 4 nodes in signal_fusion.py, got {}",
        signal_nodes.len()
    );

    // Should include fuse method
    assert!(
        signal_nodes.iter().any(|n| n.name == "fuse"),
        "Should find fuse method in signal_fusion.py"
    );
}

#[test]
fn test_arrow_batch_from_ingestion() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let result = ingest_directory(&brain_dir).expect("ingest");

    let nodes_batch = result.nodes_batch().expect("nodes batch");
    let edges_batch = result.edges_batch().expect("edges batch");

    assert!(nodes_batch.num_rows() > 10, "Should have > 10 nodes");
    assert_eq!(nodes_batch.num_columns(), 19);
    assert!(edges_batch.num_rows() > 5, "Should have > 5 edges");
    assert_eq!(edges_batch.num_columns(), 5);
}

#[test]
fn test_embedding_pipeline() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let result = ingest_directory(&brain_dir).expect("ingest");

    // Embed nodes
    let provider = HashEmbeddingProvider;
    let embeddings = embed_nodes(&result.nodes, &provider).expect("embed");

    // Should have embeddings for nodes with docstrings/signatures
    assert!(
        embeddings.len() >= 5,
        "Expected >= 5 embeddings, got {}",
        embeddings.len()
    );

    // Attach to batch
    let batch = result.nodes_batch().expect("batch");
    let updated = attach_embeddings(&batch, &embeddings).expect("attach");
    assert_eq!(updated.num_rows(), batch.num_rows());
}

#[test]
fn test_semantic_search_over_ingested_code() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let result = ingest_directory(&brain_dir).expect("ingest");

    let provider = HashEmbeddingProvider;
    let embeddings = embed_nodes(&result.nodes, &provider).expect("embed");

    // Search for signal-related code
    let results =
        semantic_search(&result.nodes, &embeddings, "signal fusion", &provider, 5).expect("search");

    assert!(
        !results.is_empty(),
        "Should find results for 'signal fusion'"
    );
    assert!(results.len() <= 5, "Should return at most 5 results");

    // Results should be sorted by score
    for w in results.windows(2) {
        assert!(w[0].score >= w[1].score);
    }
}

#[test]
fn test_metrics_computation() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let result = ingest_directory(&brain_dir).expect("ingest");

    let metrics = compute_codebase_metrics(&result.nodes);

    assert!(metrics.total_files >= 5);
    assert!(metrics.total_classes >= 3);
    assert!(metrics.total_functions >= 2);
    assert!(metrics.total_methods >= 5);
    assert!(metrics.total_loc > 0);
    assert!(metrics.avg_complexity > 0.0);
}

#[test]
fn test_complexity_query() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let result = ingest_directory(&brain_dir).expect("ingest");

    // Find high-complexity functions (complexity > 3)
    let complex = high_complexity_nodes(&result.nodes, 3);

    // The `fuse` method has multiple branches
    let has_fuse = complex.iter().any(|n| n.name == "fuse");
    assert!(
        has_fuse,
        "fuse should have complexity > 3 (branches: for, if, elif, elif)"
    );
}

#[test]
fn test_coverage_enrichment_pipeline() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let mut result = ingest_directory(&brain_dir).expect("ingest");

    // Simulate coverage data
    let coverage_json = r#"{
        "files": {
            "perception/signal_fusion.py": {
                "summary": { "percent_covered": 85.0 }
            },
            "utils/helpers.py": {
                "summary": { "percent_covered": 40.0 }
            }
        }
    }"#;

    let coverage = parse_coverage_json(coverage_json).expect("parse coverage");
    enrich_with_coverage(&mut result.nodes, &coverage);

    // Check that coverage was applied
    let low_cov = low_coverage_nodes(&result.nodes, 0.5);
    let has_helpers = low_cov.iter().any(|n| n.id.contains("utils/helpers.py"));
    assert!(has_helpers, "helpers.py nodes should have low coverage");
}

#[test]
fn test_largest_nodes_query() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let result = ingest_directory(&brain_dir).expect("ingest");

    let largest = largest_nodes(&result.nodes, 3);
    assert_eq!(largest.len(), 3);

    // Should be sorted by LOC descending
    for w in largest.windows(2) {
        assert!(w[0].loc >= w[1].loc);
    }
}

#[test]
fn test_inheritance_edges_detected() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let result = ingest_directory(&brain_dir).expect("ingest");

    let inheritance: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.predicate == CodeEdgePredicate::InheritsFrom)
        .collect();

    // MultiModalFusion inherits from SignalFusion
    assert!(!inheritance.is_empty(), "Should detect inheritance edges");
    assert!(
        inheritance
            .iter()
            .any(|e| e.source_id.contains("MultiModalFusion")),
        "MultiModalFusion should have inheritance edge"
    );
}

#[test]
fn test_ingest_rust_crate_self_ingest() {
    // Phase 5: Self-ingest validation — ingest a real NuSy crate
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("parent dir")
        .join("nusy-signal-fusion")
        .join("src");

    if !crate_dir.is_dir() {
        // Skip if the crate is not available (e.g. in CI without full checkout)
        eprintln!(
            "Skipping self-ingest test: {} not found",
            crate_dir.display()
        );
        return;
    }

    let result = ingest_directory(&crate_dir).expect("ingest nusy-signal-fusion should succeed");

    // Should find at least 5 Rust-specific CodeNodes (functions, structs, etc.)
    let rust_nodes: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind.is_rust_specific())
        .collect();
    assert!(
        rust_nodes.len() >= 5,
        "Expected >= 5 Rust-specific nodes in nusy-signal-fusion, got {} (kinds: {:?})",
        rust_nodes.len(),
        rust_nodes
            .iter()
            .map(|n| (n.kind, &n.name))
            .collect::<Vec<_>>()
    );

    // Position metadata should be populated (start_line > 0)
    let nodes_with_position: Vec<_> = rust_nodes
        .iter()
        .filter(|n| n.start_line.is_some() && n.start_line.unwrap() > 0)
        .collect();
    assert!(
        !nodes_with_position.is_empty(),
        "At least some Rust nodes should have position metadata"
    );

    // Body text should be populated for function nodes
    let fn_nodes: Vec<_> = rust_nodes
        .iter()
        .filter(|n| matches!(n.kind, CodeNodeKind::RustFn | CodeNodeKind::RustMethod))
        .collect();
    for node in &fn_nodes {
        assert!(
            node.body.is_some() && !node.body.as_ref().unwrap().is_empty(),
            "Function node {} should have non-empty body",
            node.id
        );
    }

    // No panics during parsing (we got here, so no panics)
    // Check for parse errors
    if !result.errors.is_empty() {
        eprintln!("Parse errors (non-fatal): {:?}", result.errors);
    }

    // Print summary for visibility
    eprintln!("{}", result.summary());
}

#[test]
fn test_import_edges_detected() {
    let dir = create_test_project();
    let brain_dir = dir.path().join("brain");
    let result = ingest_directory(&brain_dir).expect("ingest");

    let imports: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.predicate == CodeEdgePredicate::Imports)
        .collect();

    assert!(!imports.is_empty(), "Should detect import edges");
}
