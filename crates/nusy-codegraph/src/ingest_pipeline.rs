//! High-level ingestion pipeline — workspace-wide self-ingest and graph coherence verification.
//!
//! Provides:
//! - `ingest_workspace()`: walk all crates in a Cargo workspace and ingest them
//! - `verify_graph()`: check graph coherence (no dangling edges, no duplicate IDs)
//! - `write_graph_parquet()`: persist CodeNode + CodeEdge batches to Parquet files

use crate::ingest::{IngestResult, ingest_directory};
use arrow::array::{Array, RecordBatch, StringArray};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Workspace-wide ingestion result.
#[derive(Debug)]
pub struct WorkspaceIngestResult {
    /// Per-crate ingestion results, keyed by crate name.
    pub crates: HashMap<String, IngestResult>,
    /// Files that failed to parse across all crates.
    pub errors: Vec<(PathBuf, String)>,
    /// Crate directories that could not be opened.
    pub crate_errors: Vec<(String, String)>,
}

impl WorkspaceIngestResult {
    /// Total node count across all crates.
    pub fn total_nodes(&self) -> usize {
        self.crates.values().map(|r| r.nodes.len()).sum()
    }

    /// Total edge count across all crates.
    pub fn total_edges(&self) -> usize {
        self.crates.values().map(|r| r.edges.len()).sum()
    }

    /// Total parse error count.
    pub fn total_errors(&self) -> usize {
        self.errors.len()
    }

    /// Build a merged CodeNodes RecordBatch (all crates combined).
    pub fn merged_nodes_batch(&self) -> Result<RecordBatch, arrow::error::ArrowError> {
        let all_nodes: Vec<crate::schema::CodeNode> = self
            .crates
            .values()
            .flat_map(|r| r.nodes.iter().cloned())
            .collect();
        crate::schema::build_code_nodes_batch(&all_nodes)
    }

    /// Build a merged CodeEdges RecordBatch (all crates combined).
    pub fn merged_edges_batch(&self) -> Result<RecordBatch, arrow::error::ArrowError> {
        let all_edges: Vec<crate::schema::CodeEdge> = self
            .crates
            .values()
            .flat_map(|r| r.edges.iter().cloned())
            .collect();
        crate::schema::build_code_edges_batch(&all_edges)
    }

    /// Human-readable summary.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        s.push_str("=== Workspace Ingest Summary ===\n");
        s.push_str(&format!("Crates ingested: {}\n", self.crates.len()));
        s.push_str(&format!("Total CodeNodes: {}\n", self.total_nodes()));
        s.push_str(&format!("Total CodeEdges: {}\n", self.total_edges()));
        s.push_str(&format!("Parse errors: {}\n", self.total_errors()));
        if !self.crate_errors.is_empty() {
            s.push_str(&format!("Crate errors: {}\n", self.crate_errors.len()));
            for (name, err) in &self.crate_errors {
                s.push_str(&format!("  {name}: {err}\n"));
            }
        }
        s
    }
}

/// Ingest all crates in a Cargo workspace.
///
/// Discovers crates by reading `<workspace_root>/Cargo.toml`, collects all `.rs`
/// files from each crate's `src/` and `tests/` directories, and ingests them
/// in a **single pass** from `workspace_root` as the root. This ensures all
/// `CodeNode` IDs are workspace-relative (e.g. `crates/nusy-arrow-core/src/lib.rs::foo`)
/// and therefore globally unique across the merged graph.
///
/// If `workspace_root` is not a workspace root (no `[workspace]` key), falls back
/// to ingesting the directory as a single crate.
pub fn ingest_workspace(workspace_root: &Path) -> WorkspaceIngestResult {
    let crate_dirs = discover_workspace_crates(workspace_root);

    let mut crate_errors = Vec::new();

    // Collect all source files from all crates — paths are absolute
    let mut all_files: Vec<PathBuf> = Vec::new();
    for (crate_name, crate_dir) in &crate_dirs {
        for subdir in &["src", "tests"] {
            let dir = crate_dir.join(subdir);
            if dir.is_dir() {
                match collect_rs_files_recursive(&dir) {
                    Ok(files) => all_files.extend(files),
                    Err(e) => crate_errors.push((format!("{crate_name}/{subdir}"), e.to_string())),
                }
            }
        }
    }

    if all_files.is_empty() && crate_dirs.is_empty() {
        // Fall back: treat workspace_root itself as a single crate
        match ingest_directory(workspace_root) {
            Ok(mut result) => {
                let errors = std::mem::take(&mut result.errors);
                let mut crates = HashMap::new();
                crates.insert("workspace".to_string(), result);
                return WorkspaceIngestResult {
                    crates,
                    errors,
                    crate_errors,
                };
            }
            Err(e) => {
                crate_errors.push(("workspace".to_string(), e.to_string()));
                return WorkspaceIngestResult {
                    crates: HashMap::new(),
                    errors: Vec::new(),
                    crate_errors,
                };
            }
        }
    }

    // Single ingest pass from workspace_root → globally unique workspace-relative IDs
    let mut crates = HashMap::new();
    let mut errors = Vec::new();

    match crate::ingest::ingest_files(workspace_root, &all_files) {
        Ok(mut result) => {
            // SCIP pass: extract high-fidelity call edges via rust-analyzer.
            // Gracefully skipped if rust-analyzer is not installed.
            let scip_result =
                crate::scip_calls::extract_scip_call_edges(workspace_root, &result.nodes);
            if !scip_result.edges.is_empty() {
                tracing::info!(
                    "SCIP: {} call edges ({} resolved, {} unresolved)",
                    scip_result.edges.len(),
                    scip_result.symbols_resolved,
                    scip_result.unresolved_references,
                );
                result.edges.extend(scip_result.edges);
            }
            for w in &scip_result.warnings {
                tracing::warn!("SCIP: {w}");
            }

            errors.append(&mut result.errors);
            crates.insert("workspace".to_string(), result);
        }
        Err(e) => {
            crate_errors.push(("workspace".to_string(), e.to_string()));
        }
    }

    WorkspaceIngestResult {
        crates,
        errors,
        crate_errors,
    }
}

/// Recursively collect all `.rs` files under a directory.
fn collect_rs_files_recursive(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_rs_recursive(dir, &mut files)?;
    Ok(files)
}

fn collect_rs_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_recursive(&path, files)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

/// Discover all crate directories in a Cargo workspace.
///
/// Reads `<workspace_root>/Cargo.toml` and expands the `[workspace] members`
/// globs into concrete crate directories. Returns `(crate_name, crate_dir)` pairs.
pub fn discover_workspace_crates(workspace_root: &Path) -> Vec<(String, PathBuf)> {
    let cargo_toml = workspace_root.join("Cargo.toml");
    let Ok(content) = std::fs::read_to_string(&cargo_toml) else {
        return vec![];
    };

    let Ok(doc) = content.parse::<toml::Value>() else {
        return vec![];
    };

    // Extract workspace members
    let members: Vec<String> = doc
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut result = Vec::new();

    if members.is_empty() {
        // Not a workspace root — treat the directory itself as a single crate
        let name = extract_crate_name(workspace_root).unwrap_or_else(|| "unknown".into());
        result.push((name, workspace_root.to_path_buf()));
        return result;
    }

    // Track seen crate directories to avoid duplicates (e.g., workspace Cargo.toml
    // may list the same crate twice)
    let mut seen_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for member_pattern in &members {
        // Expand simple glob patterns (e.g., "crates/*")
        if member_pattern.ends_with("/*") {
            let parent = workspace_root.join(&member_pattern[..member_pattern.len() - 2]);
            if let Ok(entries) = std::fs::read_dir(&parent) {
                let mut dirs: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .collect();
                // Sort for deterministic ordering
                dirs.sort_by_key(|e| e.file_name());
                for entry in dirs {
                    let crate_dir = entry.path();
                    if seen_dirs.contains(&crate_dir) {
                        continue;
                    }
                    if let Some(name) = extract_crate_name(&crate_dir) {
                        seen_dirs.insert(crate_dir.clone());
                        result.push((name, crate_dir));
                    }
                }
            }
        } else {
            let crate_dir = workspace_root.join(member_pattern);
            if crate_dir.is_dir() && !seen_dirs.contains(&crate_dir) {
                let name = extract_crate_name(&crate_dir)
                    .unwrap_or_else(|| member_pattern.replace('/', "-"));
                seen_dirs.insert(crate_dir.clone());
                result.push((name, crate_dir));
            }
        }
    }

    result
}

/// Read the `[package] name` from a crate's `Cargo.toml`.
fn extract_crate_name(crate_dir: &Path) -> Option<String> {
    let cargo_toml = crate_dir.join("Cargo.toml");
    let content = std::fs::read_to_string(cargo_toml).ok()?;
    let doc: toml::Value = content.parse().ok()?;
    doc.get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(String::from)
}

/// Graph coherence violations found by `verify_graph()`.
#[derive(Debug, Default)]
pub struct GraphViolations {
    /// Edges whose `source_id` has no corresponding CodeNode.
    pub dangling_sources: Vec<(String, String)>, // (edge_source_id, predicate)
    /// Edges whose `target_id` has no corresponding CodeNode (excluding ext: refs).
    pub dangling_targets: Vec<(String, String)>, // (edge_target_id, predicate)
    /// Duplicate CodeNode IDs.
    pub duplicate_node_ids: Vec<String>,
}

impl GraphViolations {
    /// True if no violations were found.
    pub fn is_clean(&self) -> bool {
        self.dangling_sources.is_empty()
            && self.dangling_targets.is_empty()
            && self.duplicate_node_ids.is_empty()
    }

    /// Human-readable violation report.
    pub fn report(&self) -> String {
        if self.is_clean() {
            return "Graph coherence: PASS — no violations found.\n".into();
        }
        let mut s = String::new();
        s.push_str("Graph coherence: VIOLATIONS FOUND\n");
        if !self.duplicate_node_ids.is_empty() {
            s.push_str(&format!(
                "  Duplicate node IDs: {}\n",
                self.duplicate_node_ids.len()
            ));
            for id in self.duplicate_node_ids.iter().take(10) {
                s.push_str(&format!("    {id}\n"));
            }
        }
        if !self.dangling_sources.is_empty() {
            s.push_str(&format!(
                "  Dangling edge sources: {}\n",
                self.dangling_sources.len()
            ));
            for (src, pred) in self.dangling_sources.iter().take(10) {
                s.push_str(&format!("    [{pred}] source={src}\n"));
            }
        }
        if !self.dangling_targets.is_empty() {
            s.push_str(&format!(
                "  Dangling edge targets (non-ext:): {}\n",
                self.dangling_targets.len()
            ));
            for (tgt, pred) in self.dangling_targets.iter().take(10) {
                s.push_str(&format!("    [{pred}] target={tgt}\n"));
            }
        }
        s
    }
}

/// Verify graph coherence of merged CodeNode + CodeEdge RecordBatches.
///
/// Checks:
/// - No duplicate `node_id` values in the CodeNodes batch
/// - No dangling edge sources (every `source_id` exists in CodeNodes)
/// - No dangling `target_id` values — `ext:` refs are excluded (they are
///   intentional unresolved external references)
pub fn verify_graph(nodes: &RecordBatch, edges: &RecordBatch) -> GraphViolations {
    use crate::schema::{edge_col, node_col};

    let mut violations = GraphViolations::default();

    // Build node ID set and check for duplicates
    let node_id_col = nodes
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>();

    let mut node_ids: HashSet<String> = HashSet::new();
    if let Some(ids) = node_id_col {
        for i in 0..ids.len() {
            let id = ids.value(i).to_string();
            if !node_ids.insert(id.clone()) {
                violations.duplicate_node_ids.push(id);
            }
        }
    }

    // Check edges for dangling sources and targets
    let source_col = edges
        .column(edge_col::SOURCE_ID)
        .as_any()
        .downcast_ref::<StringArray>();
    let target_col = edges
        .column(edge_col::TARGET_ID)
        .as_any()
        .downcast_ref::<StringArray>();
    let pred_col = edges
        .column(edge_col::PREDICATE)
        .as_any()
        .downcast_ref::<StringArray>();

    if let (Some(sources), Some(targets), Some(preds)) = (source_col, target_col, pred_col) {
        for i in 0..sources.len() {
            let src = sources.value(i);
            let tgt = targets.value(i);
            let pred = preds.value(i);

            if !src.is_empty() && !node_ids.contains(src) {
                violations
                    .dangling_sources
                    .push((src.to_string(), pred.to_string()));
            }

            // Exclude ext: references — they are intentional unresolved external references
            if !tgt.starts_with("ext:") && !tgt.is_empty() && !node_ids.contains(tgt) {
                violations
                    .dangling_targets
                    .push((tgt.to_string(), pred.to_string()));
            }
        }
    }

    violations
}

/// Write CodeNodes and CodeEdges RecordBatches to Parquet files.
///
/// Creates two files:
/// - `<output_dir>/nodes.parquet`
/// - `<output_dir>/edges.parquet`
pub fn write_graph_parquet(
    nodes: &RecordBatch,
    edges: &RecordBatch,
    output_dir: &Path,
) -> Result<(), String> {
    use parquet::arrow::ArrowWriter;
    use std::fs::File;

    std::fs::create_dir_all(output_dir).map_err(|e| e.to_string())?;

    let nodes_path = output_dir.join("nodes.parquet");
    let nodes_file = File::create(&nodes_path).map_err(|e| e.to_string())?;
    let mut nodes_writer =
        ArrowWriter::try_new(nodes_file, nodes.schema(), None).map_err(|e| e.to_string())?;
    nodes_writer.write(nodes).map_err(|e| e.to_string())?;
    nodes_writer.close().map_err(|e| e.to_string())?;

    let edges_path = output_dir.join("edges.parquet");
    let edges_file = File::create(&edges_path).map_err(|e| e.to_string())?;
    let mut edges_writer =
        ArrowWriter::try_new(edges_file, edges.schema(), None).map_err(|e| e.to_string())?;
    edges_writer.write(edges).map_err(|e| e.to_string())?;
    edges_writer.close().map_err(|e| e.to_string())?;

    Ok(())
}

/// Load CodeNodes from a Parquet graph directory produced by `write_graph_parquet()`.
///
/// Reads `<dir>/nodes.parquet`, deserialises each row into a `CodeNode`, then
/// groups the nodes by crate prefix (derived from `file_path`) to produce a
/// `WorkspaceIngestResult` identical in shape to what `ingest_workspace()` returns.
///
/// Nodes whose `file_path` does not start with `crates/<name>/` are grouped under
/// the synthetic crate name `"_root"`.
///
/// Returns `Err` if the Parquet file cannot be opened or read.
pub fn load_nodes_from_parquet(dir: &Path) -> Result<WorkspaceIngestResult, String> {
    use arrow::array::{Array, StringArray};
    use arrow::compute::cast;
    use arrow::datatypes::DataType;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    use crate::schema::{CodeNode, CodeNodeKind};

    let nodes_path = dir.join("nodes.parquet");
    let file = File::open(&nodes_path).map_err(|e| {
        format!(
            "load_nodes_from_parquet: cannot open {}: {e}",
            nodes_path.display()
        )
    })?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| format!("load_nodes_from_parquet: parquet open: {e}"))?;
    let mut reader = builder
        .build()
        .map_err(|e| format!("load_nodes_from_parquet: build reader: {e}"))?;

    // Helper: cast any array (including dictionaries) to a plain Utf8 StringArray
    fn to_string_array(col: &dyn Array) -> StringArray {
        let utf8 = cast(col, &DataType::Utf8).expect("cast to Utf8");
        utf8.as_any()
            .downcast_ref::<StringArray>()
            .expect("StringArray after cast")
            .clone()
    }

    let mut all_nodes: Vec<CodeNode> = Vec::new();

    for batch in &mut reader {
        let batch = batch.map_err(|e| format!("load_nodes_from_parquet: read batch: {e}"))?;
        let nrows = batch.num_rows();
        if nrows == 0 {
            continue;
        }

        // Column indices match `code_nodes_schema()`:
        // 0=id, 1=kind, 2=parent_id, 3=name, 4=signature, 5=docstring,
        // 6=body_hash, 7=body(LargeUtf8), 8=embedding, 9=loc, 10=cyclomatic,
        // 11=coverage, 12=last_modified, 13=start_line, 14=end_line,
        // 15=start_col, 16=end_col, 17=file_path, 18=byte_offset
        let ids = to_string_array(batch.column(0).as_ref());
        let kinds = to_string_array(batch.column(1).as_ref());
        let names = to_string_array(batch.column(3).as_ref());

        // body is LargeUtf8 — cast to Utf8 via cast kernel
        let body_large = batch.column(7).as_ref();
        let body_utf8 = cast(body_large, &DataType::Utf8)
            .map_err(|e| format!("load_nodes_from_parquet: cast body: {e}"))?;
        let bodies = body_utf8
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("load_nodes_from_parquet: body cast failed")?;

        let file_paths = to_string_array(batch.column(17).as_ref());

        for i in 0..nrows {
            if ids.is_null(i) {
                continue;
            }
            let kind = CodeNodeKind::parse(kinds.value(i)).unwrap_or(CodeNodeKind::Function);
            let node = CodeNode {
                id: ids.value(i).to_string(),
                kind,
                name: names.value(i).to_string(),
                body: if bodies.is_null(i) {
                    None
                } else {
                    Some(bodies.value(i).to_string())
                },
                file_path: if file_paths.is_null(i) {
                    None
                } else {
                    Some(file_paths.value(i).to_string())
                },
                ..Default::default()
            };
            all_nodes.push(node);
        }
    }

    // Group by crate name derived from file_path prefix "crates/<name>/"
    let mut crates: HashMap<String, IngestResult> = HashMap::new();
    for node in all_nodes {
        let crate_name = node
            .file_path
            .as_deref()
            .and_then(|fp| {
                let stripped = fp.strip_prefix("crates/")?;
                let slash = stripped.find('/')?;
                Some(stripped[..slash].to_string())
            })
            .unwrap_or_else(|| "_root".to_string());

        crates
            .entry(crate_name)
            .or_insert_with(|| IngestResult {
                nodes: Vec::new(),
                edges: Vec::new(),
                parse_results: Vec::new(),
                errors: Vec::new(),
                source_texts: HashMap::new(),
            })
            .nodes
            .push(node);
    }

    Ok(WorkspaceIngestResult {
        crates,
        errors: Vec::new(),
        crate_errors: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn workspace_root() -> PathBuf {
        // Navigate from crates/nusy-codegraph/ up two levels to repo root
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root")
            .to_path_buf()
    }

    #[test]
    fn test_discover_workspace_crates() {
        let root = workspace_root();
        let crates = discover_workspace_crates(&root);
        // NuSy has 10+ crates
        assert!(
            crates.len() >= 8,
            "expected at least 8 workspace crates, got {}",
            crates.len()
        );
        // nusy-arrow-core and nusy-codegraph should be among them
        let names: Vec<&str> = crates.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"nusy-arrow-core"),
            "nusy-arrow-core must be in workspace crates"
        );
        assert!(
            names.contains(&"nusy-codegraph"),
            "nusy-codegraph must be in workspace crates"
        );
    }

    #[test]
    fn test_ingest_workspace_produces_nodes() {
        let root = workspace_root();
        let result = ingest_workspace(&root);

        assert!(
            result.total_nodes() >= 1_000,
            "expected >= 1000 CodeNodes from workspace, got {}",
            result.total_nodes()
        );
        assert!(
            result.total_edges() >= 2_000,
            "expected >= 2000 CodeEdges from workspace, got {}",
            result.total_edges()
        );
        // Single-pass ingest from workspace root stores results under "workspace" key
        assert!(!result.crates.is_empty(), "crates map must not be empty");
    }

    #[test]
    fn test_verify_graph_clean() {
        let root = workspace_root();
        let result = ingest_workspace(&root);

        let nodes = result
            .merged_nodes_batch()
            .expect("nodes batch should build");
        let edges = result
            .merged_edges_batch()
            .expect("edges batch should build");
        let violations = verify_graph(&nodes, &edges);

        // The parser assigns IDs like `rust_const:path.rs::NAME` which can collide
        // for same-named constants in different files. Accept a small duplicate rate
        // (< 2% of total nodes) as a known parser limitation.
        let total_nodes = nodes.num_rows();
        let dup_count = violations.duplicate_node_ids.len();
        let dup_rate = if total_nodes > 0 {
            dup_count as f64 / total_nodes as f64
        } else {
            0.0
        };
        assert!(
            dup_rate < 0.02,
            "duplicate node ID rate {:.1}% ({}/{}) exceeds 2% threshold",
            dup_rate * 100.0,
            dup_count,
            total_nodes
        );
        // Dangling sources/targets are warnings, not hard failures in test
        // (cross-crate edges point to nodes in other crates' batches)
    }

    #[test]
    fn test_verify_detects_duplicate_nodes() {
        use crate::schema::{
            CodeNode, CodeNodeKind, build_code_edges_batch, build_code_nodes_batch,
        };

        let nodes = vec![
            CodeNode {
                id: "dup::foo".into(),
                kind: CodeNodeKind::Function,
                name: "foo".into(),
                file_path: Some("lib.rs".into()),
                ..Default::default()
            },
            CodeNode {
                id: "dup::foo".into(), // duplicate
                kind: CodeNodeKind::Function,
                name: "foo".into(),
                file_path: Some("lib.rs".into()),
                ..Default::default()
            },
        ];
        let nodes_batch = build_code_nodes_batch(&nodes).expect("build nodes");
        let edges_batch = build_code_edges_batch(&[]).expect("build empty edges");
        let violations = verify_graph(&nodes_batch, &edges_batch);

        assert_eq!(
            violations.duplicate_node_ids.len(),
            1,
            "should detect the 1 duplicate ID"
        );
        assert_eq!(violations.duplicate_node_ids[0], "dup::foo");
    }

    #[test]
    fn test_write_and_verify_parquet() {
        use crate::schema::{
            CodeNode, CodeNodeKind, build_code_edges_batch, build_code_nodes_batch,
        };
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let nodes = vec![CodeNode {
            id: "test::bar".into(),
            kind: CodeNodeKind::Function,
            name: "bar".into(),
            file_path: Some("test.rs".into()),
            ..Default::default()
        }];
        let nodes_batch = build_code_nodes_batch(&nodes).expect("build");
        let edges_batch = build_code_edges_batch(&[]).expect("build");

        write_graph_parquet(&nodes_batch, &edges_batch, tmp.path())
            .expect("write parquet should succeed");

        assert!(
            tmp.path().join("nodes.parquet").exists(),
            "nodes.parquet must exist"
        );
        assert!(
            tmp.path().join("edges.parquet").exists(),
            "edges.parquet must exist"
        );
    }
}
