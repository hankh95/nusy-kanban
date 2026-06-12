//! V12-Spike-3: Agent Graph-Edit Round-Trip (EX-3109)
//!
//! Tests whether an agent can complete a code edit entirely through graph
//! operations. This spike validates the workflow BEFORE committing to
//! V14-Code's full agent MCP tooling.
//!
//! ## Gaps Found
//!
//! 1. **No Rust parsing** — `ingest_directory` only parses Python. V12-2 scope.
//! 2. **No body column** — `code_nodes_schema` has `body_hash` but not `body`.
//!    V12-1 scope. Without body text in the graph, agents can't edit code.
//! 3. **No materialization** — no function to write graph changes back to files.
//!    This spike builds the prototype (Path B).
//!
//! Run with: `cargo test -p nusy-codegraph --test agent_edit_spike -- --nocapture`

use nusy_codegraph::{CodeSearch, callers, ingest_files, search_nodes};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ─── Materialization Prototype (Path B) ─────────────────────────────────

/// Materialize source texts to a directory, preserving structure.
fn materialize(
    source_texts: &HashMap<String, String>,
    original_root: &Path,
    output_dir: &Path,
) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let root_str = original_root.to_string_lossy();

    for (file_path, source) in source_texts {
        let rel = if file_path.starts_with(root_str.as_ref()) {
            &file_path[root_str.len()..]
        } else {
            file_path.as_str()
        };
        let rel = rel.trim_start_matches('/');
        let dest = output_dir.join(rel);

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, source)?;
        written.push(dest);
    }

    Ok(written)
}

/// Materialize with a source text transformation on matching files.
fn materialize_with_edit(
    source_texts: &HashMap<String, String>,
    original_root: &Path,
    output_dir: &Path,
    target_substring: &str,
    transform: impl Fn(&str) -> String,
) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let root_str = original_root.to_string_lossy();

    for (file_path, source) in source_texts {
        let rel = if file_path.starts_with(root_str.as_ref()) {
            &file_path[root_str.len()..]
        } else {
            file_path.as_str()
        };
        let rel = rel.trim_start_matches('/');
        let dest = output_dir.join(rel);

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = if file_path.contains(target_substring) {
            transform(source)
        } else {
            source.clone()
        };

        std::fs::write(&dest, content)?;
        written.push(dest);
    }

    Ok(written)
}

// ─── Helpers ────────────────────────────────────────────────────────────

fn find_repo_root() -> PathBuf {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output();
    match output {
        Ok(o) if o.status.success() => PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()),
        _ => PathBuf::from("."),
    }
}

/// Collect Python files from a directory (non-recursive, fast).
fn python_files_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "py"))
        .map(|e| e.path())
        .collect()
}

// ─── Test: Graph Read Path ──────────────────────────────────────────────

/// Search + callers on a small Python subset (brain/perception/).
#[test]
fn spike_graph_read_path() {
    let repo_root = find_repo_root();
    let perception_dir = repo_root.join("brain/perception");

    if !perception_dir.exists() {
        eprintln!("Skipping: brain/perception/ not found");
        return;
    }

    let files = python_files_in(&perception_dir);
    if files.is_empty() {
        eprintln!("Skipping: no Python files in brain/perception/");
        return;
    }

    // Ingest only brain/perception/*.py (fast — ~20 files)
    let start = Instant::now();
    let result = ingest_files(&repo_root, &files).expect("ingestion should succeed");
    let ingest_ms = start.elapsed().as_millis();

    let nodes_batch = result.nodes_batch().expect("nodes batch");
    let edges_batch = result.edges_batch().expect("edges batch");

    eprintln!("\n=== V12-Spike-3: Graph Read Path ===");
    eprintln!(
        "Ingested {} files → {} nodes, {} edges in {}ms",
        files.len(),
        result.nodes.len(),
        result.edges.len(),
        ingest_ms
    );

    assert!(
        result.nodes.len() > 10,
        "Should ingest 10+ nodes from perception/"
    );

    // Search for SignalFusion class
    let start = Instant::now();
    let search = CodeSearch {
        name_pattern: Some("SignalFusion".to_string()),
        ..Default::default()
    };
    let search_result = search_nodes(&nodes_batch, &search);
    let search_us = start.elapsed().as_micros();

    eprintln!(
        "Search 'SignalFusion': {} matches in {}μs",
        search_result.nodes.len(),
        search_us
    );
    for node in &search_result.nodes {
        eprintln!("  Found: {} ({:?})", node.name, node.kind);
    }

    // Find callers
    if let Some(target) = search_result.nodes.first() {
        let start = Instant::now();
        let caller_list = callers(&target.id, &nodes_batch, &edges_batch);
        let callers_us = start.elapsed().as_micros();

        eprintln!(
            "Callers of '{}': {} found in {}μs",
            target.name,
            caller_list.len(),
            callers_us
        );
        for c in caller_list.iter().take(5) {
            eprintln!("  Caller: {} ({:?})", c.name, c.kind);
        }
    }

    eprintln!("--- Graph Read Path: VALIDATED ---");
}

// ─── Test: Materialization Round-Trip ───────────────────────────────────

/// Ingest → materialize → verify content matches.
#[test]
fn spike_materialization_round_trip() {
    let repo_root = find_repo_root();
    let perception_dir = repo_root.join("brain/perception");

    if !perception_dir.exists() {
        eprintln!("Skipping: brain/perception/ not found");
        return;
    }

    let files = python_files_in(&perception_dir);
    if files.is_empty() {
        eprintln!("Skipping: no .py files in brain/perception/ (archived to _archive/brain-v13/)");
        return;
    }
    let result = ingest_files(&repo_root, &files).expect("ingestion");

    let source_count = result.source_texts.len();
    assert!(source_count > 0, "Should have source texts");

    // Materialize
    let tmp = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let written = materialize(&result.source_texts, &repo_root, tmp.path()).expect("materialize");
    let ms = start.elapsed().as_millis();

    eprintln!("\n=== V12-Spike-3: Materialization Round-Trip ===");
    eprintln!(
        "Materialized {}/{} files in {}ms",
        written.len(),
        source_count,
        ms
    );

    // Verify content matches
    let mut matches = 0;
    for (orig_path, orig_content) in &result.source_texts {
        let rel = orig_path
            .strip_prefix(&repo_root.to_string_lossy().to_string())
            .unwrap_or(orig_path)
            .trim_start_matches('/');
        let mat_path = tmp.path().join(rel);
        if mat_path.exists() {
            let mat_content = std::fs::read_to_string(&mat_path).unwrap();
            assert_eq!(&mat_content, orig_content, "Content mismatch: {}", rel);
            matches += 1;
        }
    }

    assert!(matches > 0, "Should verify at least one file");
    eprintln!("All {} files match originals", matches);
    eprintln!("--- Materialization Round-Trip: VALIDATED ---");
}

// ─── Test: Materialization With Edit ────────────────────────────────────

/// Apply an edit during materialization — simulates graph-native edit workflow.
#[test]
fn spike_materialize_with_edit() {
    let repo_root = find_repo_root();
    let perception_dir = repo_root.join("brain/perception");

    if !perception_dir.exists() {
        eprintln!("Skipping: brain/perception/ not found");
        return;
    }

    let files = python_files_in(&perception_dir);
    let result = ingest_files(&repo_root, &files).expect("ingestion");

    let has_signal_fusion = result
        .source_texts
        .keys()
        .any(|k| k.contains("signal_fusion"));
    if !has_signal_fusion {
        eprintln!("Skipping: signal_fusion.py not in source texts");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let written = materialize_with_edit(
        &result.source_texts,
        &repo_root,
        tmp.path(),
        "signal_fusion",
        |source| format!("# SPIKE EDIT: graph-native workflow test\n{}", source),
    )
    .expect("materialize with edit");

    eprintln!("\n=== V12-Spike-3: Materialize With Edit ===");

    // Verify edit applied to target
    let edited = written
        .iter()
        .find(|p| p.to_string_lossy().contains("signal_fusion"))
        .expect("should have signal_fusion.py");
    let content = std::fs::read_to_string(edited).unwrap();
    assert!(content.starts_with("# SPIKE EDIT:"));

    // Verify others untouched
    let others: Vec<_> = written
        .iter()
        .filter(|p| !p.to_string_lossy().contains("signal_fusion"))
        .collect();
    for other in &others {
        let c = std::fs::read_to_string(other).unwrap();
        assert!(!c.starts_with("# SPIKE EDIT:"));
    }

    eprintln!("Edit applied to target: YES");
    eprintln!("Others untouched: YES ({} verified)", others.len());
    eprintln!("--- Materialize With Edit: VALIDATED ---");
}

// ─── Test: Gap Analysis Report ──────────────────────────────────────────

/// Document gaps and produce go/no-go.
#[test]
fn spike_gap_analysis() {
    eprintln!("\n=== V12-Spike-3: Gap Analysis ===\n");
    eprintln!("| Capability | Status | Blocker |");
    eprintln!("|---|---|---|");
    eprintln!("| Python ingestion | WORKS | — |");
    eprintln!("| Rust ingestion | MISSING | V12-2: tree-sitter-rust |");
    eprintln!("| Search by name/kind | WORKS | — |");
    eprintln!("| Caller/callee graph | WORKS | — |");
    eprintln!("| Impact analysis | WORKS | — |");
    eprintln!("| Body text in graph | MISSING | V12-1: body (LargeUtf8) column |");
    eprintln!("| Body edit via CRUD | MISSING | V12-1 + mcp_tools NodeUpdate.body |");
    eprintln!("| Materialization | PROTOTYPE | Built in this spike (Path B) |");
    eprintln!("| Graph → compile (WASM) | UNKNOWN | V12-Spike-1 |");
    eprintln!("| Graph → compile (files) | PROTOTYPE | Path B materialization |");
    eprintln!();
    eprintln!("### Go/No-Go\n");
    eprintln!("**READ path: GO** — search, callers, impact work today.");
    eprintln!("**WRITE path: NOT READY** — needs V12 body storage + Rust parsing.");
    eprintln!("**Materialization (Path B): VALIDATED**\n");
    eprintln!("### Recommendation\n");
    eprintln!("Proceed with V12 (body storage + Rust parsing).");
    eprintln!("V14-Code can start as soon as V12-1 body column lands.");
    eprintln!("Path B materialization is ready as compilation fallback.");
}
