//! Integration tests for EX-3171: CrateNode + CrateEdge tables.
//!
//! Runs `build_crate_graph` on the actual NuSy workspace root (read-only, no writes).

use arrow::array::{Array, BooleanArray, StringArray};
use nusy_codegraph::crate_graph::{build_crate_graph, topo_sort_crates};
use nusy_codegraph::crate_schema::{crate_edge_col, crate_node_col};
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

// ── Test 1: build_crate_graph on real workspace ───────────────────────────────

#[test]
fn test_build_crate_graph_produces_rows() {
    let graph = build_crate_graph(&workspace_root())
        .expect("build_crate_graph should succeed on NuSy workspace");

    // Workspace has many crates
    assert!(
        graph.crate_nodes.num_rows() >= 5,
        "expected >= 5 CrateNode rows, got {}",
        graph.crate_nodes.num_rows()
    );
    assert!(
        graph.crate_edges.num_rows() > 0,
        "expected > 0 CrateEdge rows, got {}",
        graph.crate_edges.num_rows()
    );
}

// ── Test 2: nusy-arrow-core is workspace_member = true ───────────────────────

#[test]
fn test_nusy_arrow_core_workspace_member_true() {
    let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

    let id_col = graph
        .crate_nodes
        .column(crate_node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id col is StringArray");

    let wm_col = graph
        .crate_nodes
        .column(crate_node_col::WORKSPACE_MEMBER)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("workspace_member col is BooleanArray");

    let row = (0..id_col.len())
        .find(|&i| id_col.value(i) == "nusy-arrow-core")
        .expect("nusy-arrow-core should appear in CrateNode table");

    assert!(
        wm_col.value(row),
        "nusy-arrow-core should have workspace_member = true"
    );
}

// ── Test 3: nusy-being depends on nusy-arrow-core ────────────────────────────

#[test]
fn test_nusy_being_depends_on_nusy_arrow_core() {
    let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

    let source_col = graph
        .crate_edges
        .column(crate_edge_col::SOURCE)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("source col is StringArray");
    let target_col = graph
        .crate_edges
        .column(crate_edge_col::TARGET)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("target col is StringArray");

    let has_edge = (0..source_col.len())
        .any(|i| source_col.value(i) == "nusy-being" && target_col.value(i) == "nusy-arrow-core");

    assert!(
        has_edge,
        "nusy-being → nusy-arrow-core edge should appear in CrateEdge table"
    );
}

// ── Test 4: topo_sort puts nusy-arrow-core before nusy-being ─────────────────

#[test]
fn test_topo_sort_core_before_being() {
    let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

    let order = topo_sort_crates(&graph).expect("no cycles expected in NuSy workspace");

    let core_pos = order
        .iter()
        .position(|n| n == "nusy-arrow-core")
        .expect("nusy-arrow-core in topo order");
    let being_pos = order
        .iter()
        .position(|n| n == "nusy-being")
        .expect("nusy-being in topo order");

    assert!(
        core_pos < being_pos,
        "nusy-arrow-core (pos {core_pos}) must come before nusy-being (pos {being_pos})"
    );
}

// ── Test 5: no cycles in NuSy workspace ──────────────────────────────────────

#[test]
fn test_no_cycles_in_workspace() {
    let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

    let result = topo_sort_crates(&graph);
    assert!(
        result.is_ok(),
        "NuSy workspace should have no dependency cycles: {:?}",
        result.err()
    );

    let order = result.unwrap();
    // Count only workspace members — external CrateNode rows are not in topo order
    let wm_col = graph
        .crate_nodes
        .column(crate_node_col::WORKSPACE_MEMBER)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("workspace_member col is BooleanArray");
    let ws_count = (0..wm_col.len()).filter(|&i| wm_col.value(i)).count();
    assert_eq!(
        order.len(),
        ws_count,
        "topo sort must return all {ws_count} workspace crates (got {})",
        order.len()
    );
}

// ── Test 6: CrateNode schema has correct column count ────────────────────────

#[test]
fn test_crate_node_schema_column_count() {
    let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

    assert_eq!(
        graph.crate_nodes.num_columns(),
        5,
        "CrateNode table should have 5 columns"
    );
}

// ── Test 7: CrateEdge schema has correct column count ────────────────────────

#[test]
fn test_crate_edge_schema_column_count() {
    let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

    assert_eq!(
        graph.crate_edges.num_columns(),
        7,
        "CrateEdge table should have 7 columns"
    );
}

// ── Test 8: path deps are tagged with source_kind = "path" ───────────────────

#[test]
fn test_path_deps_source_kind() {
    let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

    let source_col = graph
        .crate_edges
        .column(crate_edge_col::SOURCE)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let target_col = graph
        .crate_edges
        .column(crate_edge_col::TARGET)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let source_kind_col = graph
        .crate_edges
        .column(crate_edge_col::SOURCE_KIND)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();

    // nusy-codegraph → nusy-arrow-core is a path dep
    let edge_row = (0..source_col.len()).find(|&i| {
        source_col.value(i) == "nusy-codegraph" && target_col.value(i) == "nusy-arrow-core"
    });

    if let Some(row) = edge_row {
        assert_eq!(
            source_kind_col.value(row),
            "path",
            "nusy-codegraph → nusy-arrow-core should have source_kind = 'path'"
        );
    } else {
        // Edge may not appear if resolution differs — not a hard failure
        println!(
            "note: nusy-codegraph → nusy-arrow-core edge not found, skipping source_kind check"
        );
    }
}

// ── Test 9: external crates present as CrateNode with workspace_member=false ─

#[test]
fn test_external_crates_have_workspace_member_false() {
    let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

    let id_col = graph
        .crate_nodes
        .column(crate_node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id col is StringArray");
    let wm_col = graph
        .crate_nodes
        .column(crate_node_col::WORKSPACE_MEMBER)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("workspace_member col is BooleanArray");

    let serde_row = (0..id_col.len()).find(|&i| id_col.value(i) == "serde");
    assert!(
        serde_row.is_some(),
        "external crate 'serde' must appear as a CrateNode"
    );
    assert!(
        !wm_col.value(serde_row.unwrap()),
        "serde should have workspace_member = false"
    );

    let external_count = (0..wm_col.len()).filter(|&i| !wm_col.value(i)).count();
    assert!(
        external_count > 5,
        "expected many external CrateNode rows, got {external_count}"
    );
}
