//! query-self-graph — Run demonstration queries against the NuSy self-graph Parquet snapshot.
//!
//! Answers 3 queries from EX-3170 Phase 4:
//!   1. What functions does nusy-being call?
//!   2. What crates have edges into nusy-arrow-core?
//!   3. How many test functions are in the workspace?

use arrow::array::{Array, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;

fn main() {
    let graph_dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "research/shared/self-graph".to_string()),
    );

    let nodes = read_parquet(&graph_dir.join("nodes.parquet"));
    let edges = read_parquet(&graph_dir.join("edges.parquet"));

    let node_rows = nodes.num_rows();
    let edge_rows = edges.num_rows();
    println!("Self-graph loaded: {node_rows} nodes, {edge_rows} edges\n");

    // Extract node columns — use cast to Utf8 to handle Dictionary columns
    let node_ids_col = nodes.column(0); // id — Utf8
    let node_kinds_col = nodes.column(1); // kind — Dictionary<Int8, Utf8>
    let node_names_col = nodes.column(3); // name — Utf8

    // Cast dictionary columns to StringArray via Arrow cast kernel
    let node_ids = cast_to_string(node_ids_col);
    let node_kinds = cast_to_string(node_kinds_col);
    let node_names = cast_to_string(node_names_col);

    // Edge columns: source_id=0, target_id=1, predicate=2
    let edge_sources = cast_to_string(edges.column(0));
    let edge_targets = cast_to_string(edges.column(1));
    let edge_preds = cast_to_string(edges.column(2));

    // ─── Query 1: What does nusy-being call? ─────────────────────────────────
    println!("=== Query 1: What does nusy-being call? ===");
    let being_nodes: HashSet<String> = (0..node_ids.len())
        .filter(|&i| node_ids.value(i).contains("nusy-being"))
        .map(|i| node_ids.value(i).to_string())
        .collect();

    let mut callees: Vec<String> = (0..edge_sources.len())
        .filter(|&i| being_nodes.contains(edge_sources.value(i)) && edge_preds.value(i) == "calls")
        .map(|i| edge_targets.value(i).to_string())
        .collect();
    callees.sort();
    callees.dedup();

    println!("nusy-being nodes: {}", being_nodes.len());
    println!("Functions called from nusy-being: {}", callees.len());
    for c in callees.iter().take(15) {
        println!("  {c}");
    }
    if callees.len() > 15 {
        println!("  ... ({} more)", callees.len() - 15);
    }
    println!();

    // ─── Query 2: What crates have edges into nusy-arrow-core? ────────────────
    println!("=== Query 2: Crates with edges into nusy-arrow-core nodes ===");
    let arrow_core_nodes: HashSet<String> = (0..node_ids.len())
        .filter(|&i| node_ids.value(i).contains("nusy-arrow-core"))
        .map(|i| node_ids.value(i).to_string())
        .collect();

    let mut dep_crates: HashSet<String> = HashSet::new();
    for i in 0..edge_targets.len() {
        let tgt = edge_targets.value(i);
        let src = edge_sources.value(i);
        if arrow_core_nodes.contains(tgt)
            && let Some(c) = extract_crate_from_id(src).filter(|c| c != "nusy-arrow-core")
        {
            dep_crates.insert(c);
        }
    }
    let mut dep_list: Vec<String> = dep_crates.into_iter().collect();
    dep_list.sort();
    println!(
        "Crates with code edges into nusy-arrow-core nodes: {}",
        dep_list.len()
    );
    for c in &dep_list {
        println!("  {c}");
    }
    println!();

    // ─── Query 3: How many test functions? ────────────────────────────────────
    println!("=== Query 3: Test functions in the workspace ===");
    let test_count = (0..node_ids.len())
        .filter(|&i| {
            let name = node_names.value(i);
            name.starts_with("test_") || node_ids.value(i).contains("/tests/")
        })
        .count();
    let rust_fn_count = (0..node_kinds.len())
        .filter(|&i| node_kinds.value(i) == "rust_function")
        .count();
    let rust_method_count = (0..node_kinds.len())
        .filter(|&i| node_kinds.value(i) == "rust_method")
        .count();
    println!("rust_function nodes: {rust_fn_count}");
    println!("rust_method nodes: {rust_method_count}");
    println!("Test functions (name starts with test_ or in tests/): {test_count}");
    println!();

    // ─── Kind breakdown ────────────────────────────────────────────────────────
    println!("=== Node Kind Breakdown ===");
    let mut kind_counts: HashMap<String, usize> = HashMap::new();
    for i in 0..node_kinds.len() {
        *kind_counts
            .entry(node_kinds.value(i).to_string())
            .or_insert(0) += 1;
    }
    let mut kinds: Vec<(String, usize)> = kind_counts.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1));
    for (k, c) in kinds {
        println!("  {k}: {c}");
    }

    // ─── Edge predicate breakdown ──────────────────────────────────────────────
    println!("\n=== Edge Predicate Breakdown ===");
    let mut pred_counts: HashMap<String, usize> = HashMap::new();
    for i in 0..edge_preds.len() {
        *pred_counts
            .entry(edge_preds.value(i).to_string())
            .or_insert(0) += 1;
    }
    let mut preds: Vec<(String, usize)> = pred_counts.into_iter().collect();
    preds.sort_by(|a, b| b.1.cmp(&a.1));
    for (p, c) in preds {
        println!("  {p}: {c}");
    }
}

fn extract_crate_from_id(node_id: &str) -> Option<String> {
    // Node IDs: rust_fn:crates/nusy-arrow-core/src/lib.rs::foo
    let path_part = node_id.split_once(':')?.1;
    let parts: Vec<&str> = path_part.splitn(4, '/').collect();
    if parts.len() >= 3 && parts[0] == "crates" {
        Some(parts[1].to_string())
    } else {
        None
    }
}

fn cast_to_string(col: &dyn Array) -> StringArray {
    use arrow::compute::cast;
    use arrow::datatypes::DataType;
    let utf8 = cast(col, &DataType::Utf8).expect("cast to Utf8");
    utf8.as_any()
        .downcast_ref::<StringArray>()
        .expect("downcast to StringArray")
        .clone()
}

fn read_parquet(path: &std::path::Path) -> arrow::array::RecordBatch {
    let file = File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap_or_else(|e| panic!("parquet builder {}: {e}", path.display()));
    let mut reader = builder
        .build()
        .unwrap_or_else(|e| panic!("build reader {}: {e}", path.display()));
    let mut batches = Vec::new();
    for batch in &mut reader {
        batches.push(batch.expect("read batch"));
    }
    assert!(!batches.is_empty(), "no data in {}", path.display());
    arrow::compute::concat_batches(&batches[0].schema(), &batches).expect("concat")
}
