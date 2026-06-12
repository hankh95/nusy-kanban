//! Integration tests for Python-specific ingestion (EX-3172 / V12b-1).
//!
//! Validates:
//! 1. `PythonParser` emits Python-specific kinds with position metadata
//! 2. `PythonModuleResolver` indexes files and resolves imports
//! 3. `ingest_python_directory` successfully parses brain-v13 archive
//!    (≥ 1,000 Python nodes, < 5% parse failure rate)

use nusy_codegraph::{CodeNodeKind, PythonModuleResolver, PythonParser, ingest_python_directory};
use std::path::Path;

// ─── PythonParser integration tests ─────────────────────────────────────────

#[test]
fn test_python_parser_on_realistic_file() {
    let source = r#"
"""Cognitive signal fusion module."""
from __future__ import annotations

import logging
from typing import Any, Dict, List, Optional

logger = logging.getLogger(__name__)


class SignalFusionBrain:
    """Fuses cognitive signals from multiple assessors."""

    def __init__(self, config: Dict[str, Any]) -> None:
        """Initialize the fusion brain."""
        self.config = config
        self._cache: Dict[str, float] = {}

    @property
    def domain(self) -> str:
        """Return the cognitive domain."""
        return self.config.get("domain", "general")

    @classmethod
    def from_config(cls, path: str) -> "SignalFusionBrain":
        """Load from config file."""
        return cls({})

    async def fuse(self, signals: List[Any]) -> Dict[str, float]:
        """Async signal fusion pipeline."""
        result: Dict[str, float] = {}
        for signal in signals:
            if signal.weight > 0.0 and signal.valid:
                result[signal.name] = signal.score
            elif signal.fallback is not None:
                result[signal.name] = signal.fallback
        return result

    def _compute(self, x: float, y: float) -> float:
        """Private computation helper."""
        return x * y + 1.0


def standalone_helper(x: int) -> int:
    """Top-level utility function."""
    return x + 1


async def async_pipeline(items: List[Any]) -> List[Any]:
    """Top-level async function."""
    return [item for item in items if item is not None]
"#;

    let mut parser = PythonParser::new().expect("parser init");
    let path = std::path::PathBuf::from("brain/perception/signal_fusion.py");
    let result = parser.parse_file(&path, source).expect("parse");

    // Verify Python-specific kinds
    let class_nodes: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::PythonClass)
        .collect();
    assert_eq!(class_nodes.len(), 1, "one class expected");
    assert_eq!(class_nodes[0].name, "SignalFusionBrain");

    let methods: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::PythonMethod)
        .collect();
    // __init__, from_config, _compute (non-async, non-property regular methods)
    assert!(
        methods.len() >= 2,
        "expected >= 2 PythonMethod nodes, got {}",
        methods.len()
    );

    let props: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::PythonProperty)
        .collect();
    assert_eq!(props.len(), 1, "expected 1 PythonProperty");
    assert_eq!(props[0].name, "domain");

    let async_nodes: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::PythonAsync)
        .collect();
    // fuse (method) + async_pipeline (top-level)
    assert!(
        async_nodes.len() >= 2,
        "expected >= 2 PythonAsync nodes, got {}",
        async_nodes.len()
    );

    let funcs: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::PythonFunction)
        .collect();
    assert!(
        funcs.iter().any(|f| f.name == "standalone_helper"),
        "should have standalone_helper as PythonFunction"
    );

    // Verify all nodes have position metadata
    for node in &result.nodes {
        assert!(node.start_line.is_some(), "{} missing start_line", node.id);
        assert!(node.file_path.is_some(), "{} missing file_path", node.id);
        assert!(
            node.byte_offset.is_some(),
            "{} missing byte_offset",
            node.id
        );
    }

    // No generic kinds (Function/Method/Class/Module) should be emitted
    for node in &result.nodes {
        assert!(
            !matches!(
                node.kind,
                CodeNodeKind::Function
                    | CodeNodeKind::Method
                    | CodeNodeKind::Class
                    | CodeNodeKind::Module
            ),
            "node {} has generic kind {:?}",
            node.id,
            node.kind
        );
    }
}

// ─── PythonModuleResolver integration tests ──────────────────────────────────

#[test]
fn test_resolver_on_temp_package() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // Create a realistic package structure
    let files = [
        ("brain/__init__.py", ""),
        ("brain/perception/__init__.py", ""),
        (
            "brain/perception/signal_fusion.py",
            "from .utils import helper\nfrom brain.utils import top_helper",
        ),
        ("brain/perception/utils.py", "def helper(): pass"),
        ("brain/utils.py", "def top_helper(): pass"),
    ];
    for (rel, content) in &files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        std::fs::write(&path, content).expect("write");
    }

    let resolver = PythonModuleResolver::from_root(root).expect("build resolver");

    // Absolute resolution
    assert!(resolver.knows_module("brain.perception.signal_fusion"));
    assert!(resolver.knows_module("brain.perception.utils"));
    assert!(resolver.knows_module("brain.utils"));

    let resolved = resolver
        .resolve_import("brain.perception.utils", None)
        .expect("should resolve brain.perception.utils");
    assert!(
        resolved.ends_with("brain/perception/utils.py"),
        "resolved to wrong path: {resolved:?}"
    );

    // Relative resolution
    let from_file = Path::new("brain/perception/signal_fusion.py");
    let resolved_rel = resolver.resolve_import(".utils", Some(from_file));
    assert!(
        resolved_rel.is_some(),
        "should resolve relative .utils import"
    );
}

// ─── brain-v13 full ingestion test ──────────────────────────────────────────

/// Full ingestion of the archived brain-v13 Python codebase.
///
/// This test validates the DoD for EX-3172:
/// - ≥ 1,000 Python CodeNodes parsed
/// - < 5% parse failure rate across 1,554 files
///
/// The test is skipped if the archive is not present (CI environments without
/// the full archive). Run locally with:
/// ```
/// cargo test -p nusy-codegraph --test python_ingest_test test_brain_v13_full_ingest
/// ```
#[test]
fn test_brain_v13_full_ingest() {
    // Compute path relative to workspace root (two levels up from crates/nusy-codegraph/)
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");
    let brain_v13 = workspace_root.join("_archive/brain-v13");
    if !brain_v13.is_dir() {
        // Skip gracefully in environments without the archive
        eprintln!("SKIP: _archive/brain-v13 not present");
        return;
    }

    let result =
        ingest_python_directory(&brain_v13).expect("ingest_python_directory should succeed");

    let total_files = result.parse_results.len() + result.errors.len();
    let error_count = result.errors.len();
    let error_pct = if total_files > 0 {
        (error_count as f64 / total_files as f64) * 100.0
    } else {
        0.0
    };

    eprintln!(
        "brain-v13 ingest: {} nodes, {} edges, {} files parsed, {} errors ({:.1}%)",
        result.nodes.len(),
        result.edges.len(),
        result.parse_results.len(),
        error_count,
        error_pct,
    );

    // DoD: ≥ 1,000 Python CodeNodes
    assert!(
        result.nodes.len() >= 1_000,
        "Expected >= 1,000 Python nodes, got {}",
        result.nodes.len()
    );

    // DoD: < 5% parse failures
    assert!(
        error_pct < 5.0,
        "Parse failure rate {:.1}% exceeds 5% limit ({} errors out of {} files)",
        error_pct,
        error_count,
        total_files,
    );

    // Verify Python-specific kinds are used (not generic)
    let python_class_count = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::PythonClass)
        .count();
    let python_func_count = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::PythonFunction)
        .count();
    let python_method_count = result
        .nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::PythonMethod)
        .count();

    eprintln!(
        "  PythonClass: {}, PythonFunction: {}, PythonMethod: {}",
        python_class_count, python_func_count, python_method_count
    );

    assert!(python_class_count > 0, "should have PythonClass nodes");
    assert!(
        python_func_count + python_method_count > 0,
        "should have PythonFunction or PythonMethod nodes"
    );

    // Verify position metadata is populated on non-file nodes
    let nodes_with_position = result
        .nodes
        .iter()
        .filter(|n| n.start_line.is_some())
        .count();
    let position_coverage = nodes_with_position as f64 / result.nodes.len() as f64;
    eprintln!(
        "  Position coverage: {:.1}% ({}/{} nodes)",
        position_coverage * 100.0,
        nodes_with_position,
        result.nodes.len()
    );
    assert!(
        position_coverage >= 0.8,
        "Expected >= 80% nodes to have position metadata, got {:.1}%",
        position_coverage * 100.0,
    );

    // Print summary for comment recording
    eprintln!("{}", result.summary());
}
