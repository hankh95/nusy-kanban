//! Batch ingestion — walk a directory tree, parse all Python files, build Arrow tables.
//!
//! Two ingestion modes are available:
//!
//! - **Generic** (`ingest_directory` / `ingest_files`): uses `parser::parse_python_file`,
//!   emitting language-agnostic `CodeNodeKind` variants (File, Module, Class, Function, etc.).
//!   Backward-compatible with existing callers.
//!
//! - **Python-specific** (`ingest_python_directory` / `Language::Python`): uses
//!   `PythonParser`, emitting Python-specific kinds (PythonFunction, PythonClass, etc.)
//!   with full position metadata populated.
//!
//! Usage:
//! ```ignore
//! // Generic (backward compat)
//! let result = ingest_directory(Path::new("brain/"))?;
//! println!("{}", result.summary());
//!
//! // Python-specific with position metadata
//! let result = ingest_python_directory(Path::new("_archive/brain-v13/brain/"))?;
//! println!("{}", result.summary());
//! ```

use crate::edges::{NameResolver, extract_edges};
use crate::parser::{ParseResult, parse_python_file};
use crate::python_parser::{PythonParseResult, PythonParser};
use crate::rust_parser::parse_rust_file;
use crate::schema::{
    CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind, build_code_edges_batch,
    build_code_nodes_batch,
};
use arrow::array::RecordBatch;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Language selection for the ingest pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// Use the generic parser (language-agnostic `CodeNodeKind` variants).
    ///
    /// Backward-compatible. Does not populate position metadata.
    Generic,
    /// Use the Python-specific parser (`PythonParser`).
    ///
    /// Emits `PythonFunction`, `PythonClass`, etc. with full position metadata.
    Python,
}

/// Errors from ingestion.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    Parse(#[from] crate::parser::ParseError),

    #[error("Python parser error: {0}")]
    PythonParse(#[from] crate::python_parser::PythonParserError),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Directory not found: {0}")]
    DirNotFound(String),
}

pub type Result<T> = std::result::Result<T, IngestError>;

/// Result of a full directory ingestion.
#[derive(Debug)]
pub struct IngestResult {
    /// All parsed CodeNodes.
    pub nodes: Vec<CodeNode>,
    /// All extracted CodeEdges.
    pub edges: Vec<CodeEdge>,
    /// Per-file parse results (for call edge extraction).
    pub parse_results: Vec<ParseResult>,
    /// Files that failed to parse (path, error message).
    pub errors: Vec<(PathBuf, String)>,
    /// Source text by file path (for call edge extraction).
    pub source_texts: HashMap<String, String>,
}

impl IngestResult {
    /// Build CodeNodes RecordBatch from ingestion results.
    pub fn nodes_batch(&self) -> std::result::Result<RecordBatch, arrow::error::ArrowError> {
        build_code_nodes_batch(&self.nodes)
    }

    /// Build CodeEdges RecordBatch from ingestion results.
    pub fn edges_batch(&self) -> std::result::Result<RecordBatch, arrow::error::ArrowError> {
        build_code_edges_batch(&self.edges)
    }

    /// Human-readable summary of the ingestion.
    pub fn summary(&self) -> String {
        let mut s = String::new();

        // Node counts by kind
        s.push_str("=== CodeGraph Ingestion Summary ===\n");
        s.push_str(&format!("Total nodes: {}\n", self.nodes.len()));
        for kind in CodeNodeKind::ALL {
            let count = self.nodes.iter().filter(|n| n.kind == kind).count();
            if count > 0 {
                s.push_str(&format!("  {}: {}\n", kind.as_str(), count));
            }
        }

        // Edge counts by predicate
        s.push_str(&format!("Total edges: {}\n", self.edges.len()));
        for pred in CodeEdgePredicate::ALL {
            let count = self.edges.iter().filter(|e| e.predicate == pred).count();
            if count > 0 {
                s.push_str(&format!("  {}: {}\n", pred.as_str(), count));
            }
        }

        // Unresolved references
        let unresolved = self
            .edges
            .iter()
            .filter(|e| e.target_id.starts_with("ext:"))
            .count();
        s.push_str(&format!("Unresolved references: {}\n", unresolved));

        // Errors
        if !self.errors.is_empty() {
            s.push_str(&format!("Parse errors: {}\n", self.errors.len()));
            for (path, err) in &self.errors {
                s.push_str(&format!("  {}: {}\n", path.display(), err));
            }
        }

        s
    }
}

/// Ingest all Python and Rust files under a directory into a CodeGraph.
///
/// Walks the directory tree recursively, parses each `.py` and `.rs` file,
/// extracts cross-file edges, and returns the full graph.
pub fn ingest_directory(root: &Path) -> Result<IngestResult> {
    if !root.is_dir() {
        return Err(IngestError::DirNotFound(root.display().to_string()));
    }

    let source_files = collect_source_files(root)?;

    ingest_files(root, &source_files)
}

/// Ingest a specific list of source files (Python and Rust).
///
/// `root` is the base directory (used to compute relative paths for node IDs).
pub fn ingest_files(root: &Path, files: &[PathBuf]) -> Result<IngestResult> {
    let mut all_parse_results = Vec::new();
    let mut errors = Vec::new();
    let mut source_texts = HashMap::new();

    for file_path in files {
        let rel_path = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_path_buf();

        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

        match std::fs::read_to_string(file_path) {
            Ok(source) => {
                let path_str = rel_path.display().to_string();
                source_texts.insert(path_str, source.clone());

                let parse_result = match ext {
                    "py" => parse_python_file(&rel_path, &source),
                    "rs" => parse_rust_file(&rel_path, &source),
                    _ => continue, // Skip unsupported extensions
                };

                match parse_result {
                    Ok(result) => {
                        all_parse_results.push(result);
                    }
                    Err(e) => {
                        errors.push((file_path.clone(), e.to_string()));
                    }
                }
            }
            Err(e) => {
                errors.push((file_path.clone(), format!("{}: {}", file_path.display(), e)));
            }
        }
    }

    // Collect all nodes
    let all_nodes: Vec<CodeNode> = all_parse_results
        .iter()
        .flat_map(|r| r.nodes.clone())
        .collect();

    // Build resolver and extract edges
    let resolver = NameResolver::from_nodes(&all_nodes);
    let mut edges = extract_edges(&all_parse_results, &resolver);

    // Extract call edges — prefer SCIP when rust-analyzer is available (Rust),
    // fall back to text scanning for Python or when SCIP is unavailable.
    let call_edges =
        crate::edges::extract_call_edges(&all_parse_results, &resolver, &source_texts, Some(root));
    edges.extend(call_edges);

    // Extract cross-file edges for Rust crates using module resolution
    if let Some(mut module_resolver) = crate::module_resolver::RustModuleResolver::from_crate(root)
    {
        module_resolver.index_nodes(&all_nodes);
        let cross_edges =
            crate::edges::extract_cross_file_edges(&all_parse_results, &module_resolver);
        edges.extend(cross_edges);
    }

    Ok(IngestResult {
        nodes: all_nodes,
        edges,
        parse_results: all_parse_results,
        errors,
        source_texts,
    })
}

// ─── Python-specific ingestion (Language::Python) ───────────────────────────

/// Ingest all Python files under a directory using the Python-specific parser.
///
/// Unlike `ingest_directory`, this uses `PythonParser` which:
/// - Emits Python-specific `CodeNodeKind` variants (PythonFunction, PythonClass, etc.)
/// - Populates position metadata for all nodes (start_line, end_line, etc.)
/// - Emits PythonDecorator, PythonImport, PythonAsync, PythonProperty nodes
pub fn ingest_python_directory(root: &Path) -> Result<IngestResult> {
    if !root.is_dir() {
        return Err(IngestError::DirNotFound(root.display().to_string()));
    }
    let py_files = collect_python_files(root)?;
    ingest_python_files(root, &py_files)
}

/// Ingest a specific list of Python files using the Python-specific parser.
///
/// `root` is used to compute relative paths for node IDs.
pub fn ingest_python_files(root: &Path, files: &[PathBuf]) -> Result<IngestResult> {
    let mut parser = PythonParser::new()?;

    let mut all_parse_results: Vec<ParseResult> = Vec::new();
    let mut py_results: Vec<PythonParseResult> = Vec::new();
    let mut errors = Vec::new();
    let mut source_texts = HashMap::new();

    for file_path in files {
        let rel_path = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_path_buf();

        match std::fs::read_to_string(file_path) {
            Ok(source) => {
                let path_str = rel_path.display().to_string();
                source_texts.insert(path_str, source.clone());

                match parser.parse_file(&rel_path, &source) {
                    Ok(result) => {
                        py_results.push(result);
                    }
                    Err(e) => {
                        errors.push((file_path.clone(), e.to_string()));
                    }
                }
            }
            Err(e) => {
                errors.push((file_path.clone(), format!("{}: {}", file_path.display(), e)));
            }
        }
    }

    // Collect nodes from Python-specific results
    let all_nodes: Vec<CodeNode> = py_results.iter().flat_map(|r| r.nodes.clone()).collect();

    // Convert PythonParseResult imports to ParseResult-compatible form for edge extraction
    // We build minimal ParseResult entries for the import graph.
    for py_result in &py_results {
        // Build a minimal ParseResult so edge extraction can process imports
        let parse_result = ParseResult {
            nodes: py_result.nodes.clone(),
            imports: py_result.imports.clone(),
        };
        all_parse_results.push(parse_result);
    }

    let resolver = NameResolver::from_nodes(&all_nodes);
    let mut edges = extract_edges(&all_parse_results, &resolver);
    // Python: text scanning only (SCIP is Rust-only).
    let call_edges =
        crate::edges::extract_call_edges(&all_parse_results, &resolver, &source_texts, None);
    edges.extend(call_edges);

    Ok(IngestResult {
        nodes: all_nodes,
        edges,
        parse_results: all_parse_results,
        errors,
        source_texts,
    })
}

/// Recursively collect all `.py` files under a directory.
fn collect_python_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_source_files_recursive(dir, &mut files)?;
    // Filter to .py only
    files.retain(|f| f.extension().is_some_and(|e| e == "py"));
    files.sort();
    Ok(files)
}

/// Recursively collect all `.py` and `.rs` files under a directory.
fn collect_source_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_source_files_recursive(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_source_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Skip common non-source directories
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with('.')
                || name == "__pycache__"
                || name == "node_modules"
                || name == ".git"
                || name == "venv"
                || name == ".venv"
                || name == "target"
            {
                continue;
            }
            collect_source_files_recursive(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|ext| ext == "py" || ext == "rs")
        {
            files.push(path);
        }
    }
    Ok(())
}

/// Query nodes by file path (returns nodes whose ID contains the path).
pub fn nodes_in_file<'a>(nodes: &'a [CodeNode], file_path: &str) -> Vec<&'a CodeNode> {
    nodes.iter().filter(|n| n.id.contains(file_path)).collect()
}

/// Query callers of a function/method by name.
///
/// Uses exact segment matching on `::` boundaries to avoid false positives
/// (e.g., searching for "fuse" won't match "defuse").
pub fn callers_of<'a>(edges: &'a [CodeEdge], target_name: &str) -> Vec<&'a CodeEdge> {
    edges
        .iter()
        .filter(|e| {
            if e.predicate != CodeEdgePredicate::Calls {
                return false;
            }
            // Check if the last segment of the target_id matches exactly
            if let Some(last_segment) = e.target_id.rsplit("::").next() {
                last_segment == target_name
            } else {
                // No :: separator — check if the whole ID ends with the name
                e.target_id.ends_with(target_name)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ingest_files_from_source() {
        // Create temp files
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_a = dir.path().join("module_a.py");
        let file_b = dir.path().join("module_b.py");

        std::fs::write(
            &file_a,
            r#"
"""Module A."""

from module_b import helper

class Processor:
    """Processes data."""
    def process(self, data):
        """Process data."""
        return helper(data)

def standalone():
    """Standalone function."""
    return 42
"#,
        )
        .expect("write a");

        std::fs::write(
            &file_b,
            r#"
"""Module B."""

def helper(x):
    """Help with x."""
    return x * 2

def another():
    """Another function."""
    return helper(10)
"#,
        )
        .expect("write b");

        let result = ingest_directory(dir.path()).expect("ingest should succeed");

        // Should have nodes from both files
        assert!(
            result.nodes.len() >= 8,
            "Expected >= 8 nodes, got {}",
            result.nodes.len()
        );

        // Should have edges
        assert!(!result.edges.is_empty(), "Should have edges");

        // Should have containment edges
        let containment = result
            .edges
            .iter()
            .filter(|e| e.predicate == CodeEdgePredicate::Contains)
            .count();
        assert!(
            containment >= 4,
            "Expected >= 4 containment edges, got {}",
            containment
        );

        // Should have import edge
        let imports = result
            .edges
            .iter()
            .filter(|e| e.predicate == CodeEdgePredicate::Imports)
            .count();
        assert!(imports >= 1, "Expected >= 1 import edge, got {}", imports);

        // Summary should work
        let summary = result.summary();
        assert!(summary.contains("Total nodes:"));
        assert!(summary.contains("Total edges:"));

        // No parse errors
        assert!(result.errors.is_empty(), "Should have no parse errors");
    }

    #[test]
    fn test_ingest_builds_record_batches() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(
            dir.path().join("test.py"),
            r#"
def foo():
    """A function."""
    return 1

def bar():
    """Another function."""
    return foo()
"#,
        )
        .expect("write");

        let result = ingest_directory(dir.path()).expect("ingest");
        let nodes_batch = result.nodes_batch().expect("nodes batch");
        let edges_batch = result.edges_batch().expect("edges batch");

        assert!(nodes_batch.num_rows() > 0);
        assert_eq!(nodes_batch.num_columns(), 19);
        assert!(edges_batch.num_rows() > 0);
        assert_eq!(edges_batch.num_columns(), 5);
    }

    #[test]
    fn test_ingest_dir_not_found() {
        let result = ingest_directory(Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn test_ingest_skips_pycache() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let pycache = dir.path().join("__pycache__");
        std::fs::create_dir(&pycache).expect("create pycache");
        std::fs::write(pycache.join("cached.py"), "x = 1").expect("write cached");
        std::fs::write(dir.path().join("real.py"), "def foo(): pass").expect("write real");

        let result = ingest_directory(dir.path()).expect("ingest");

        // Should only have nodes from real.py, not __pycache__/cached.py
        let file_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == CodeNodeKind::File)
            .collect();
        assert_eq!(file_nodes.len(), 1, "Should only parse real.py");
    }

    #[test]
    fn test_nodes_in_file_query() {
        let nodes = vec![
            CodeNode {
                id: "func:brain/utils.py::helper".to_string(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "helper".to_string(),
                signature: None,
                docstring: None,
                body_hash: None,
                body: None,
                loc: None,
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/main.py::main".to_string(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "main".to_string(),
                signature: None,
                docstring: None,
                body_hash: None,
                body: None,
                loc: None,
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
        ];

        let utils_nodes = nodes_in_file(&nodes, "brain/utils.py");
        assert_eq!(utils_nodes.len(), 1);
        assert_eq!(utils_nodes[0].name, "helper");
    }

    #[test]
    fn test_callers_of_query() {
        let edges = vec![
            CodeEdge {
                source_id: "func:a.py::caller".to_string(),
                target_id: "func:b.py::target".to_string(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(1.0),
                commit_id: None,
            },
            CodeEdge {
                source_id: "func:a.py::other".to_string(),
                target_id: "func:c.py::unrelated".to_string(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(1.0),
                commit_id: None,
            },
        ];

        let callers = callers_of(&edges, "target");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].source_id, "func:a.py::caller");
    }

    #[test]
    fn test_ingest_handles_syntax_errors_gracefully() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("good.py"), "def foo(): pass").expect("write good");
        // tree-sitter is very lenient — it won't fail on most syntax errors
        // but we test that the pipeline doesn't crash
        std::fs::write(dir.path().join("weird.py"), "def (: pass").expect("write weird");

        let result = ingest_directory(dir.path()).expect("ingest should succeed");
        // Should have at least the good file's nodes
        assert!(!result.nodes.is_empty());
    }

    #[test]
    fn test_ingest_rust_files() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            r#"
use std::collections::HashMap;

pub struct Config {
    pub name: String,
}

impl Config {
    pub fn new(name: &str) -> Self {
        Config { name: name.to_string() }
    }
}

pub fn process(input: &str) -> String {
    input.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process() {
        assert_eq!(process("hello"), "HELLO");
    }
}
"#,
        )
        .expect("write rust");

        let result = ingest_directory(dir.path()).expect("ingest should succeed");

        // Should have Rust-specific nodes
        let rust_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind.is_rust_specific())
            .collect();
        assert!(
            rust_nodes.len() >= 5,
            "Expected >= 5 Rust-specific nodes, got {} (kinds: {:?})",
            rust_nodes.len(),
            rust_nodes
                .iter()
                .map(|n| (n.kind, &n.name))
                .collect::<Vec<_>>()
        );

        // Should have position metadata
        for node in &rust_nodes {
            if !matches!(node.kind, CodeNodeKind::RustUse) {
                assert!(
                    node.start_line.is_some() && node.start_line.unwrap() > 0,
                    "Rust node {} should have start_line > 0",
                    node.id
                );
            }
        }

        // Should have containment edges
        let containment = result
            .edges
            .iter()
            .filter(|e| e.predicate == CodeEdgePredicate::Contains)
            .count();
        assert!(
            containment >= 3,
            "Expected >= 3 containment edges, got {}",
            containment
        );

        // No parse errors
        assert!(
            result.errors.is_empty(),
            "Should have no parse errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_ingest_mixed_py_and_rs() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("app.py"), "def main():\n    pass\n").expect("write py");
        std::fs::write(dir.path().join("lib.rs"), "fn helper() -> i32 { 42 }\n").expect("write rs");

        let result = ingest_directory(dir.path()).expect("ingest");

        // Should have both Python and Rust file nodes
        let file_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == CodeNodeKind::File)
            .collect();
        assert_eq!(file_nodes.len(), 2, "Should have 2 file nodes");

        // Should have Python function
        let py_func = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::Function && n.name == "main");
        assert!(py_func.is_some(), "Should have Python function main");

        // Should have Rust function
        let rs_func = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustFn && n.name == "helper");
        assert!(rs_func.is_some(), "Should have Rust function helper");
    }

    #[test]
    fn test_ingest_skips_target_dir() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let target = dir.path().join("target");
        std::fs::create_dir(&target).expect("create target");
        std::fs::write(target.join("build.rs"), "fn main() {}").expect("write target file");
        std::fs::write(dir.path().join("real.rs"), "fn real() {}").expect("write real");

        let result = ingest_directory(dir.path()).expect("ingest");

        let file_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == CodeNodeKind::File)
            .collect();
        assert_eq!(
            file_nodes.len(),
            1,
            "Should only parse real.rs, not target/build.rs"
        );
    }

    #[test]
    fn test_collect_source_files_sorted() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("z.py"), "x=1").expect("write");
        std::fs::write(dir.path().join("a.py"), "x=1").expect("write");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub/m.py"), "x=1").expect("write");

        let files = collect_source_files(dir.path()).expect("collect");
        assert_eq!(files.len(), 3);
        // Should be sorted
        let names: Vec<_> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.windows(2).all(|w| w[0] <= w[1]));
    }
}
