//! SCIP-based call edge extraction.
//!
//! Uses rust-analyzer's SCIP output to derive high-fidelity `Calls` edges.
//! SCIP provides compiler-quality definition/reference data; we correlate
//! references inside function body ranges to produce call edges.
//!
//! ## Pipeline
//!
//! 1. Run `rust-analyzer scip .` on the target crate (external process)
//! 2. Parse the SCIP protobuf output
//! 3. Build a map: symbol → definition location (file + range)
//! 4. For each function body, find all reference occurrences
//! 5. Match references to known definitions → emit `CodeEdge::Calls`

use crate::schema::{CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Result of SCIP-based call extraction.
#[derive(Debug)]
pub struct ScipCallResult {
    /// Call edges derived from SCIP data.
    pub edges: Vec<CodeEdge>,
    /// Number of SCIP documents (files) processed.
    pub documents_processed: usize,
    /// Number of symbols resolved.
    pub symbols_resolved: usize,
    /// Number of references that couldn't be resolved to a CodeNode.
    pub unresolved_references: usize,
    /// Warnings/errors encountered.
    pub warnings: Vec<String>,
}

/// Run `rust-analyzer scip` on a crate directory and return the SCIP index path.
///
/// Returns `None` if rust-analyzer is not installed or fails.
pub fn generate_scip_index(crate_dir: &Path) -> Option<PathBuf> {
    let output_path = crate_dir.join("index.scip");

    let result = Command::new("rust-analyzer")
        .arg("scip")
        .arg(".")
        .arg("--output")
        .arg(&output_path)
        .current_dir(crate_dir)
        .output();

    match result {
        Ok(output) if output.status.success() => {
            if output_path.exists() {
                Some(output_path)
            } else {
                tracing::warn!("rust-analyzer scip succeeded but no output file");
                None
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // rust-analyzer writes progress to stderr even on success;
            // check if the file was actually created
            if output_path.exists() {
                Some(output_path)
            } else {
                tracing::warn!("rust-analyzer scip failed: {stderr}");
                None
            }
        }
        Err(e) => {
            tracing::info!("rust-analyzer not available, skipping SCIP: {e}");
            None
        }
    }
}

/// Extract call edges from a SCIP index file, mapping to existing CodeNodes.
pub fn extract_calls_from_scip(
    scip_path: &Path,
    nodes: &[CodeNode],
    crate_root: &Path,
) -> ScipCallResult {
    let data = match std::fs::read(scip_path) {
        Ok(d) => d,
        Err(e) => {
            return ScipCallResult {
                edges: vec![],
                documents_processed: 0,
                symbols_resolved: 0,
                unresolved_references: 0,
                warnings: vec![format!("Failed to read SCIP file: {e}")],
            };
        }
    };

    let index: scip::types::Index = match protobuf::Message::parse_from_bytes(&data) {
        Ok(idx) => idx,
        Err(e) => {
            return ScipCallResult {
                edges: vec![],
                documents_processed: 0,
                symbols_resolved: 0,
                unresolved_references: 0,
                warnings: vec![format!("Failed to parse SCIP protobuf: {e}")],
            };
        }
    };

    // Build lookup tables from our existing CodeNodes.
    let node_by_file_line = build_node_lookup(nodes);
    let node_by_name = build_name_lookup(nodes);

    // Build SCIP symbol → definition location map.
    let mut symbol_defs: HashMap<String, SymbolDef> = HashMap::new();
    let mut documents_processed = 0;

    for doc in &index.documents {
        documents_processed += 1;
        let rel_path = &doc.relative_path;

        for occ in &doc.occurrences {
            let symbol = &occ.symbol;
            if symbol.is_empty() || symbol.starts_with("local ") {
                continue;
            }

            let is_def = occ.symbol_roles & scip::types::SymbolRole::Definition as i32 != 0;
            if is_def && occ.range.len() >= 3 {
                let line = occ.range[0] as u32 + 1;
                let col = occ.range[1] as u32;
                symbol_defs.insert(
                    symbol.clone(),
                    SymbolDef {
                        file: rel_path.clone(),
                        line,
                        col,
                        symbol: symbol.clone(),
                    },
                );
            }
        }
    }

    // Now find call edges: for each function node, find references in its body range.
    let mut edges = Vec::new();
    let mut symbols_resolved = 0;
    let mut unresolved_references = 0;

    // Build a map of file → [(occurrence, symbol, is_reference)]
    type RefEntry = (u32, u32, u32, u32, String);
    let mut file_refs: HashMap<String, Vec<RefEntry>> = HashMap::new();
    for doc in &index.documents {
        let rel_path = &doc.relative_path;
        for occ in &doc.occurrences {
            let symbol = &occ.symbol;
            if symbol.is_empty() || symbol.starts_with("local ") {
                continue;
            }
            let is_ref = occ.symbol_roles & scip::types::SymbolRole::Definition as i32 == 0;
            if is_ref && occ.range.len() >= 3 {
                let (start_line, start_col) = (occ.range[0] as u32 + 1, occ.range[1] as u32);
                let (end_line, end_col) = if occ.range.len() >= 4 {
                    (occ.range[2] as u32, occ.range[3] as u32)
                } else {
                    (occ.range[0] as u32 + 1, occ.range[2] as u32)
                };
                file_refs.entry(rel_path.clone()).or_default().push((
                    start_line,
                    start_col,
                    end_line,
                    end_col,
                    symbol.clone(),
                ));
            }
        }
    }

    // For each function/method node, check which references fall inside its body range.
    let callable_kinds = [
        CodeNodeKind::RustFn,
        CodeNodeKind::RustMethod,
        CodeNodeKind::RustTest,
    ];

    for node in nodes {
        if !callable_kinds.contains(&node.kind) {
            continue;
        }

        let (Some(start), Some(end)) = (node.start_line, node.end_line) else {
            continue;
        };
        let Some(ref file_path) = node.file_path else {
            continue;
        };

        // Normalize path relative to crate root.
        let rel_file = file_path
            .strip_prefix(crate_root.to_str().unwrap_or(""))
            .unwrap_or(file_path)
            .trim_start_matches('/');

        let Some(refs) = file_refs.get(rel_file) else {
            continue;
        };

        // Collect unique call targets from references in this function's body.
        let mut seen_targets = std::collections::HashSet::new();

        for (ref_line, _ref_col, _end_line, _end_col, symbol) in refs {
            // Is this reference inside the function body?
            if *ref_line < start || *ref_line > end {
                continue;
            }

            // Does this symbol resolve to a known definition?
            if let Some(def) = symbol_defs.get(symbol) {
                // Try to find the target CodeNode.
                let target_id = resolve_target(def, &node_by_file_line, &node_by_name);
                if let Some(target_id) = target_id {
                    if target_id != node.id && seen_targets.insert(target_id.clone()) {
                        edges.push(CodeEdge {
                            source_id: node.id.clone(),
                            target_id,
                            predicate: CodeEdgePredicate::Calls,
                            weight: Some(1.0),
                            commit_id: None,
                        });
                        symbols_resolved += 1;
                    }
                } else {
                    unresolved_references += 1;
                }
            }
        }
    }

    // Clean up the SCIP file.
    let _ = std::fs::remove_file(scip_path);

    ScipCallResult {
        edges,
        documents_processed,
        symbols_resolved,
        unresolved_references,
        warnings: vec![],
    }
}

/// Generate SCIP index and extract call edges in one step.
///
/// Returns empty result (not error) if rust-analyzer is not available.
pub fn extract_scip_call_edges(crate_dir: &Path, nodes: &[CodeNode]) -> ScipCallResult {
    let scip_path = match generate_scip_index(crate_dir) {
        Some(p) => p,
        None => {
            return ScipCallResult {
                edges: vec![],
                documents_processed: 0,
                symbols_resolved: 0,
                unresolved_references: 0,
                warnings: vec!["rust-analyzer not available; SCIP call extraction skipped".into()],
            };
        }
    };

    extract_calls_from_scip(&scip_path, nodes, crate_dir)
}

// ── Internal helpers ────────────────────────────────────────────────────────

#[derive(Debug)]
struct SymbolDef {
    file: String,
    line: u32,
    #[allow(dead_code)]
    col: u32,
    #[allow(dead_code)]
    symbol: String,
}

/// Build a lookup: (file, line) → CodeNode ID.
fn build_node_lookup(nodes: &[CodeNode]) -> HashMap<(String, u32), String> {
    let mut map = HashMap::new();
    for node in nodes {
        if let (Some(fp), Some(line)) = (&node.file_path, node.start_line) {
            let key = (fp.clone(), line);
            map.entry(key).or_insert_with(|| node.id.clone());
        }
    }
    map
}

/// Build a lookup: short name → CodeNode ID (for fallback resolution).
fn build_name_lookup(nodes: &[CodeNode]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let callable = [
        CodeNodeKind::RustFn,
        CodeNodeKind::RustMethod,
        CodeNodeKind::RustTest,
    ];
    for node in nodes {
        if callable.contains(&node.kind) && !node.name.is_empty() {
            map.entry(node.name.clone())
                .or_insert_with(|| node.id.clone());
        }
    }
    map
}

/// Try to resolve a SCIP definition to a CodeNode ID.
fn resolve_target(
    def: &SymbolDef,
    by_file_line: &HashMap<(String, u32), String>,
    by_name: &HashMap<String, String>,
) -> Option<String> {
    // Primary: match by file path + line number.
    let key = (def.file.clone(), def.line);
    if let Some(id) = by_file_line.get(&key) {
        return Some(id.clone());
    }

    // Try without leading "src/" prefix (SCIP uses relative paths).
    if def.file.starts_with("src/") {
        let stripped = def.file.strip_prefix("src/").unwrap_or(&def.file);
        let key2 = (stripped.to_string(), def.line);
        if let Some(id) = by_file_line.get(&key2) {
            return Some(id.clone());
        }
    }

    // Fallback: extract function name from SCIP symbol and match by name.
    let name = extract_name_from_scip_symbol(&def.symbol);
    if !name.is_empty()
        && let Some(id) = by_name.get(&name)
    {
        return Some(id.clone());
    }

    None
}

/// Extract a short name from a SCIP symbol string.
///
/// SCIP symbols look like: `rust-analyzer cargo arrow-kanban 0.1.0 create_item().`
/// We want the last identifier before the `()` or `.` suffix.
fn extract_name_from_scip_symbol(symbol: &str) -> String {
    // Take the last space-separated token and strip trailing punctuation.
    symbol
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim_end_matches('.')
        .trim_end_matches("()")
        .trim_end_matches('#')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_name_from_symbol() {
        assert_eq!(
            extract_name_from_scip_symbol("rust-analyzer cargo arrow-kanban 0.1.0 create_item()."),
            "create_item"
        );
        assert_eq!(
            extract_name_from_scip_symbol("rust-analyzer cargo arrow-kanban 0.1.0 KanbanStore#"),
            "KanbanStore"
        );
        assert_eq!(
            extract_name_from_scip_symbol("rust-analyzer cargo arrow-kanban 0.1.0 crate/crud.rs/"),
            "crate/crud.rs/"
        );
        assert_eq!(extract_name_from_scip_symbol(""), "");
    }

    #[test]
    fn build_lookups_from_nodes() {
        let nodes = vec![
            CodeNode {
                id: "rust_fn:src/crud.rs::create_item".into(),
                kind: CodeNodeKind::RustFn,
                name: "create_item".into(),
                file_path: Some("src/crud.rs".into()),
                start_line: Some(10),
                end_line: Some(50),
                ..Default::default()
            },
            CodeNode {
                id: "rust_fn:src/crud.rs::move_item".into(),
                kind: CodeNodeKind::RustFn,
                name: "move_item".into(),
                file_path: Some("src/crud.rs".into()),
                start_line: Some(55),
                end_line: Some(90),
                ..Default::default()
            },
        ];

        let by_fl = build_node_lookup(&nodes);
        assert_eq!(
            by_fl.get(&("src/crud.rs".into(), 10u32)),
            Some(&"rust_fn:src/crud.rs::create_item".to_string())
        );

        let by_name = build_name_lookup(&nodes);
        assert_eq!(
            by_name.get("create_item"),
            Some(&"rust_fn:src/crud.rs::create_item".to_string())
        );
    }

    #[test]
    fn scip_result_defaults_empty() {
        let result = ScipCallResult {
            edges: vec![],
            documents_processed: 0,
            symbols_resolved: 0,
            unresolved_references: 0,
            warnings: vec![],
        };
        assert!(result.edges.is_empty());
    }
}
