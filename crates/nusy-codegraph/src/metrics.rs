//! Code metrics computation — LOC, complexity, coverage, git history.
//!
//! The parser already computes LOC and cyclomatic complexity inline.
//! This module adds:
//! - `last_modified` from `git log` output
//! - Coverage import from pytest JSON/XML reports
//! - Query helpers for filtering nodes by metric thresholds

use crate::schema::{CodeNode, CodeNodeKind};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Errors from metrics operations.
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Git error: {0}")]
    Git(String),

    #[error("JSON parse error: {0}")]
    Json(String),
}

pub type Result<T> = std::result::Result<T, MetricsError>;

/// Enrich CodeNodes with `last_modified` timestamps from git log.
///
/// Runs `git log` for each unique file path and applies the timestamp
/// to all nodes in that file.
pub fn enrich_with_git_timestamps(nodes: &mut [CodeNode], repo_root: &Path) -> Result<()> {
    // Collect unique file paths from node IDs
    let file_paths: Vec<String> = nodes
        .iter()
        .filter_map(|n| extract_file_path(&n.id))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Get last modified time for each file
    let timestamps = git_last_modified_batch(repo_root, &file_paths)?;

    // Apply timestamps to nodes
    for node in nodes.iter_mut() {
        if let Some(file_path) = extract_file_path(&node.id)
            && let Some(&ts) = timestamps.get(&file_path)
        {
            node.last_modified = Some(ts);
        }
    }

    Ok(())
}

/// Get the last modification timestamp (epoch millis) for multiple files via git.
///
/// Uses a single `git log` invocation to get timestamps for all files at once,
/// avoiding O(n) process spawns for large codebases.
fn git_last_modified_batch(
    repo_root: &Path,
    file_paths: &[String],
) -> Result<HashMap<String, i64>> {
    if file_paths.is_empty() {
        return Ok(HashMap::new());
    }

    // Single-pass: `git log --format="%ct" --name-only` with all file paths.
    // Each commit block: timestamp line, then file names, then blank line.
    let mut args = vec![
        "log".to_string(),
        "--format=format:%ct".to_string(),
        "--name-only".to_string(),
        "--".to_string(),
    ];
    args.extend(file_paths.iter().cloned());

    let output = Command::new("git")
        .args(&args)
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        return Err(MetricsError::Git("git log batch failed".to_string()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut timestamps: HashMap<String, i64> = HashMap::new();
    let mut current_ts: Option<i64> = None;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Try to parse as a timestamp (pure digits)
        if let Ok(epoch_secs) = trimmed.parse::<i64>() {
            current_ts = Some(epoch_secs * 1000);
        } else if let Some(ts) = current_ts {
            // This is a file path line — only record the FIRST (most recent) timestamp
            timestamps.entry(trimmed.to_string()).or_insert(ts);
        }
    }

    Ok(timestamps)
}

/// Extract the file path from a CodeNode ID.
///
/// IDs look like `func:brain/utils.py::helper` → `brain/utils.py`
fn extract_file_path(id: &str) -> Option<String> {
    let after_prefix = id.split_once(':').map(|(_, rest)| rest)?;
    let file_part = after_prefix
        .split_once("::")
        .map_or(after_prefix, |(f, _)| f);
    if file_part.ends_with(".py") {
        Some(file_part.to_string())
    } else {
        None
    }
}

/// Coverage data for a single file (line-level).
#[derive(Debug, Clone, Default)]
pub struct FileCoverage {
    /// Lines that were executed.
    pub covered_lines: Vec<u32>,
    /// Lines that were not executed.
    pub missing_lines: Vec<u32>,
    /// Total executable lines.
    pub total_lines: u32,
    /// Coverage percentage (0.0 to 1.0).
    pub coverage_pct: f64,
}

/// Enrich CodeNodes with coverage percentages from a coverage map.
///
/// The coverage map keys are file paths (matching node ID file paths).
pub fn enrich_with_coverage(nodes: &mut [CodeNode], coverage: &HashMap<String, FileCoverage>) {
    for node in nodes.iter_mut() {
        if let Some(file_path) = extract_file_path(&node.id)
            && let Some(cov) = coverage.get(&file_path)
        {
            node.coverage_pct = Some(cov.coverage_pct);
        }
    }
}

/// Parse a coverage.json (pytest-cov JSON format) into a file-level coverage map.
///
/// Expected format:
/// ```json
/// {
///   "files": {
///     "brain/utils.py": {
///       "summary": { "covered_lines": 42, "missing_lines": 8, "percent_covered": 84.0 }
///     }
///   }
/// }
/// ```
pub fn parse_coverage_json(json_str: &str) -> Result<HashMap<String, FileCoverage>> {
    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| MetricsError::Json(e.to_string()))?;

    let mut coverage = HashMap::new();

    let Some(files) = parsed.get("files").and_then(|f| f.as_object()) else {
        return Ok(coverage);
    };

    for (path, file_data) in files {
        if let Some(pct) = file_data
            .get("summary")
            .and_then(|s| s.get("percent_covered"))
            .and_then(|p| p.as_f64())
        {
            coverage.insert(
                path.clone(),
                FileCoverage {
                    coverage_pct: pct / 100.0,
                    ..Default::default()
                },
            );
        }
    }

    Ok(coverage)
}

// ─── Query helpers ──────────────────────────────────────────────────────────

/// Find nodes with cyclomatic complexity above a threshold.
pub fn high_complexity_nodes(nodes: &[CodeNode], threshold: i32) -> Vec<&CodeNode> {
    nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                CodeNodeKind::Function | CodeNodeKind::Method | CodeNodeKind::Test
            ) && n.cyclomatic_complexity.is_some_and(|c| c > threshold)
        })
        .collect()
}

/// Find nodes with coverage below a threshold (0.0 to 1.0).
pub fn low_coverage_nodes(nodes: &[CodeNode], threshold: f64) -> Vec<&CodeNode> {
    nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                CodeNodeKind::Function
                    | CodeNodeKind::Method
                    | CodeNodeKind::Test
                    | CodeNodeKind::Class
            ) && n.coverage_pct.is_some_and(|c| c < threshold)
        })
        .collect()
}

/// Find the largest nodes by LOC.
pub fn largest_nodes(nodes: &[CodeNode], top_k: usize) -> Vec<&CodeNode> {
    let mut sortable: Vec<&CodeNode> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                CodeNodeKind::Function | CodeNodeKind::Method | CodeNodeKind::Class
            ) && n.loc.is_some()
        })
        .collect();

    sortable.sort_by(|a, b| b.loc.cmp(&a.loc));
    sortable.truncate(top_k);
    sortable
}

/// Compute aggregate metrics for the codebase.
#[derive(Debug, Clone)]
pub struct CodebaseMetrics {
    pub total_files: usize,
    pub total_classes: usize,
    pub total_functions: usize,
    pub total_methods: usize,
    pub total_tests: usize,
    pub total_loc: i64,
    pub avg_complexity: f64,
    pub avg_coverage: Option<f64>,
}

/// Compute aggregate metrics from a set of CodeNodes.
pub fn compute_codebase_metrics(nodes: &[CodeNode]) -> CodebaseMetrics {
    let total_files = nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::File)
        .count();
    let total_classes = nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::Class)
        .count();
    let total_functions = nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::Function)
        .count();
    let total_methods = nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::Method)
        .count();
    let total_tests = nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::Test)
        .count();

    let total_loc: i64 = nodes
        .iter()
        .filter(|n| n.kind == CodeNodeKind::File)
        .filter_map(|n| n.loc.map(|l| l as i64))
        .sum();

    let complexities: Vec<f64> = nodes
        .iter()
        .filter_map(|n| n.cyclomatic_complexity.map(|c| c as f64))
        .collect();
    let avg_complexity = if complexities.is_empty() {
        0.0
    } else {
        complexities.iter().sum::<f64>() / complexities.len() as f64
    };

    let coverages: Vec<f64> = nodes.iter().filter_map(|n| n.coverage_pct).collect();
    let avg_coverage = if coverages.is_empty() {
        None
    } else {
        Some(coverages.iter().sum::<f64>() / coverages.len() as f64)
    };

    CodebaseMetrics {
        total_files,
        total_classes,
        total_functions,
        total_methods,
        total_tests,
        total_loc,
        avg_complexity,
        avg_coverage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "file:brain/signal.py".to_string(),
                kind: CodeNodeKind::File,
                parent_id: None,
                name: "signal.py".to_string(),
                signature: None,
                docstring: None,
                body_hash: None,
                body: None,
                loc: Some(200),
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/signal.py::fuse".to_string(),
                kind: CodeNodeKind::Function,
                parent_id: Some("mod:brain/signal.py".to_string()),
                name: "fuse".to_string(),
                signature: Some("def fuse(signals)".to_string()),
                docstring: None,
                body_hash: None,
                body: None,
                loc: Some(30),
                cyclomatic_complexity: Some(8),
                coverage_pct: Some(0.9),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/signal.py::simple".to_string(),
                kind: CodeNodeKind::Function,
                parent_id: Some("mod:brain/signal.py".to_string()),
                name: "simple".to_string(),
                signature: Some("def simple()".to_string()),
                docstring: None,
                body_hash: None,
                body: None,
                loc: Some(5),
                cyclomatic_complexity: Some(1),
                coverage_pct: Some(0.3),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "method:brain/store.py::Store::save".to_string(),
                kind: CodeNodeKind::Method,
                parent_id: Some("class:brain/store.py::Store".to_string()),
                name: "save".to_string(),
                signature: Some("def save(self, key, value)".to_string()),
                docstring: None,
                body_hash: None,
                body: None,
                loc: Some(15),
                cyclomatic_complexity: Some(12),
                coverage_pct: Some(0.4),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/test_signal.py::test_fuse".to_string(),
                kind: CodeNodeKind::Test,
                parent_id: None,
                name: "test_fuse".to_string(),
                signature: Some("def test_fuse()".to_string()),
                docstring: None,
                body_hash: None,
                body: None,
                loc: Some(10),
                cyclomatic_complexity: Some(2),
                coverage_pct: Some(1.0),
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    #[test]
    fn test_extract_file_path() {
        assert_eq!(
            extract_file_path("func:brain/utils.py::helper"),
            Some("brain/utils.py".to_string())
        );
        assert_eq!(
            extract_file_path("file:brain/main.py"),
            Some("brain/main.py".to_string())
        );
        assert_eq!(
            extract_file_path("method:brain/store.py::Store::save"),
            Some("brain/store.py".to_string())
        );
        assert_eq!(extract_file_path("invalid"), None);
    }

    #[test]
    fn test_high_complexity_nodes() {
        let nodes = sample_nodes();
        let high = high_complexity_nodes(&nodes, 10);
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].name, "save");
    }

    #[test]
    fn test_low_coverage_nodes() {
        let nodes = sample_nodes();
        let low = low_coverage_nodes(&nodes, 0.5);
        assert_eq!(low.len(), 2);
        let names: Vec<&str> = low.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"simple"));
        assert!(names.contains(&"save"));
    }

    #[test]
    fn test_largest_nodes() {
        let nodes = sample_nodes();
        let largest = largest_nodes(&nodes, 2);
        assert_eq!(largest.len(), 2);
        assert_eq!(largest[0].name, "fuse"); // 30 LOC
        assert_eq!(largest[1].name, "save"); // 15 LOC
    }

    #[test]
    fn test_compute_codebase_metrics() {
        let nodes = sample_nodes();
        let metrics = compute_codebase_metrics(&nodes);

        assert_eq!(metrics.total_files, 1);
        assert_eq!(metrics.total_functions, 2);
        assert_eq!(metrics.total_methods, 1);
        assert_eq!(metrics.total_tests, 1);
        assert_eq!(metrics.total_loc, 200); // from file node
        assert!(metrics.avg_complexity > 0.0);
        assert!(metrics.avg_coverage.is_some());
    }

    #[test]
    fn test_parse_coverage_json() {
        let json = r#"{
            "meta": {"version": "7.4"},
            "files": {
                "brain/signal.py": {
                    "summary": {
                        "covered_lines": 42,
                        "missing_lines": 8,
                        "percent_covered": 84.0
                    }
                },
                "brain/store.py": {
                    "summary": {
                        "covered_lines": 20,
                        "missing_lines": 30,
                        "percent_covered": 40.0
                    }
                }
            }
        }"#;

        let coverage = parse_coverage_json(json).unwrap();
        assert_eq!(coverage.len(), 2);
        assert!(
            (coverage["brain/signal.py"].coverage_pct - 0.84).abs() < 0.01,
            "Expected ~0.84, got {}",
            coverage["brain/signal.py"].coverage_pct
        );
        assert!(
            (coverage["brain/store.py"].coverage_pct - 0.40).abs() < 0.01,
            "Expected ~0.40, got {}",
            coverage["brain/store.py"].coverage_pct
        );
    }

    #[test]
    fn test_parse_coverage_json_empty() {
        let coverage = parse_coverage_json("{}").unwrap();
        assert!(coverage.is_empty());
    }

    #[test]
    fn test_enrich_with_coverage() {
        let mut nodes = sample_nodes();
        let mut coverage = HashMap::new();
        coverage.insert(
            "brain/signal.py".to_string(),
            FileCoverage {
                coverage_pct: 0.75,
                ..Default::default()
            },
        );

        enrich_with_coverage(&mut nodes, &coverage);

        // All nodes from signal.py should have 0.75 coverage
        let signal_nodes: Vec<_> = nodes
            .iter()
            .filter(|n| n.id.contains("brain/signal.py"))
            .collect();
        for node in &signal_nodes {
            assert_eq!(
                node.coverage_pct,
                Some(0.75),
                "{} should have coverage 0.75",
                node.id
            );
        }

        // Nodes from store.py should be unchanged
        let store_node = nodes.iter().find(|n| n.id.contains("brain/store.py"));
        assert!(store_node.is_some());
        // store.py was not in coverage map, so it keeps its original value
    }

    #[test]
    fn test_high_complexity_excludes_files() {
        let nodes = sample_nodes();
        let high = high_complexity_nodes(&nodes, 0);
        // Should not include the File node even though it has no complexity
        assert!(high.iter().all(|n| n.kind != CodeNodeKind::File));
    }
}
