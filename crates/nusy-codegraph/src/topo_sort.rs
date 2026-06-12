//! Topological sorters for the NuSy crate dependency graph and function call graph.
//!
//! Provides two flavours of Kahn's BFS topological sort:
//!
//! - **Crate-level** — order workspace crates for compilation.
//!   - [`sort_crates`] — flattened build order (Vec<String>)
//!   - [`sort_crates_parallel`] — layered order; crates in the same layer may build concurrently
//!
//! - **Function-level** (intra-crate) — order functions by call dependency.
//!   - [`sort_functions_in_crate`] — flattened call order (Vec<String> of node IDs)
//!   - [`sort_functions_parallel`] — layered call order
//!
//! All algorithms use named column constants (never magic indices) and return
//! `Err(String)` for cycle detection or schema mismatches.

use crate::crate_graph::CrateGraph;
use crate::crate_schema::{crate_edge_col, crate_node_col};
use crate::schema::{edge_col, node_col};
use arrow::array::{Array, BooleanArray, RecordBatch, StringArray};
use std::collections::{HashMap, HashSet, VecDeque};

// ─── Crate-level sort ────────────────────────────────────────────────────────

/// Topologically sort workspace crate names using Kahn's algorithm.
///
/// Returns crate names in build order — every dependency appears before its
/// dependents. Only workspace-internal path/workspace edges are considered;
/// crates.io deps are ignored. Returns `Err` on cycle detection.
pub fn sort_crates(graph: &CrateGraph) -> Result<Vec<String>, String> {
    crate::crate_graph::topo_sort_crates(graph)
}

/// Return independent parallel build layers for workspace crates.
///
/// Layer 0 has no intra-workspace dependencies; layer N depends only on
/// crates already in layers 0..N-1. All crates within a layer may be
/// compiled concurrently.
pub fn sort_crates_parallel(graph: &CrateGraph) -> Result<Vec<Vec<String>>, String> {
    let id_col = graph
        .crate_nodes
        .column(crate_node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CrateNode id column is not StringArray")?;

    let wm_col = graph
        .crate_nodes
        .column(crate_node_col::WORKSPACE_MEMBER)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or("CrateNode workspace_member column is not BooleanArray")?;

    let workspace_members: HashSet<String> = (0..id_col.len())
        .filter(|&i| wm_col.value(i))
        .map(|i| id_col.value(i).to_string())
        .collect();

    build_parallel_layers(&workspace_members, |layers| {
        wire_crate_edges(graph, &workspace_members, layers)
    })
}

// ─── Function-level sort ─────────────────────────────────────────────────────

/// Topologically sort functions within a crate by call dependency order.
///
/// Filters `nodes` to those whose `file_path` is under `crates/<crate_name>/`,
/// then walks `Calls` and `Uses` edges within that set (ignoring cross-crate
/// calls). Returns node IDs (column 0 = ID) in order: callees before callers.
///
/// Pass the full workspace `code_nodes` and `code_edges` RecordBatches from
/// [`crate::ingest`]. Returns `Err` on cycle detection or schema mismatch.
pub fn sort_functions_in_crate(
    crate_name: &str,
    nodes: &RecordBatch,
    edges: &RecordBatch,
) -> Result<Vec<String>, String> {
    let (node_ids, adjacency, in_degree) = build_function_graph(crate_name, nodes, edges)?;
    kahn_sort(node_ids, adjacency, in_degree)
}

/// Return independent parallel execution layers for functions within a crate.
///
/// Layer 0 has no intra-crate call dependencies; layer N is called only by
/// functions in layers N+1 and above. Functions in the same layer may
/// (conceptually) execute concurrently.
pub fn sort_functions_parallel(
    crate_name: &str,
    nodes: &RecordBatch,
    edges: &RecordBatch,
) -> Result<Vec<Vec<String>>, String> {
    let (node_ids, adjacency, in_degree) = build_function_graph(crate_name, nodes, edges)?;
    kahn_layers(node_ids, adjacency, in_degree)
}

// ─── Parallelism statistics ───────────────────────────────────────────────────

/// Statistics about the parallel layer decomposition of a workspace.
#[derive(Debug)]
pub struct ParallelismStats {
    /// Number of parallel layers (critical path length in layers).
    pub layer_count: usize,
    /// Maximum number of crates in any single layer.
    pub max_layer_width: usize,
    /// Crates on the critical path (longest dependency chain), ordered.
    pub critical_path: Vec<String>,
}

/// Compute parallelism statistics for the workspace crate build graph.
pub fn crate_parallelism_stats(graph: &CrateGraph) -> Result<ParallelismStats, String> {
    let layers = sort_crates_parallel(graph)?;
    let max_layer_width = layers.iter().map(|l| l.len()).max().unwrap_or(0);
    let layer_count = layers.len();

    // Critical path: longest chain — find source and sink with maximum
    // depth-first path length over the DAG.
    let id_col = graph
        .crate_nodes
        .column(crate_node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CrateNode id column is not StringArray")?;

    let wm_col = graph
        .crate_nodes
        .column(crate_node_col::WORKSPACE_MEMBER)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or("CrateNode workspace_member column is not BooleanArray")?;

    let workspace_members: HashSet<String> = (0..id_col.len())
        .filter(|&i| wm_col.value(i))
        .map(|i| id_col.value(i).to_string())
        .collect();

    // Build reverse adjacency (target → sources) for critical path tracing
    let mut fwd: HashMap<String, Vec<String>> = HashMap::new();
    for name in &workspace_members {
        fwd.entry(name.clone()).or_default();
    }
    wire_crate_edges(graph, &workspace_members, &mut fwd);

    // Compute longest path to each node (in topo order)
    let flat = sort_crates(graph)?;
    let mut dist: HashMap<String, usize> = HashMap::new();
    let mut prev: HashMap<String, String> = HashMap::new();
    for name in &flat {
        dist.insert(name.clone(), 0);
    }
    for name in &flat {
        let d = dist[name];
        for nbr in fwd.get(name).into_iter().flatten() {
            if workspace_members.contains(nbr) {
                let entry = dist.entry(nbr.clone()).or_insert(0);
                if d + 1 > *entry {
                    *entry = d + 1;
                    prev.insert(nbr.clone(), name.clone());
                }
            }
        }
    }

    // Trace back from deepest node
    let sink = dist
        .iter()
        .max_by_key(|(_, v)| *v)
        .map(|(k, _)| k.clone())
        .unwrap_or_default();

    let mut path = Vec::new();
    let mut cur = sink.clone();
    loop {
        path.push(cur.clone());
        match prev.get(&cur) {
            Some(p) => cur = p.clone(),
            None => break,
        }
    }
    path.reverse();

    Ok(ParallelismStats {
        layer_count,
        max_layer_width,
        critical_path: path,
    })
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Wire workspace-internal crate edges into `adjacency[source] = Vec<target>`.
fn wire_crate_edges(
    graph: &CrateGraph,
    workspace_members: &HashSet<String>,
    adjacency: &mut HashMap<String, Vec<String>>,
) {
    let source_col = graph
        .crate_edges
        .column(crate_edge_col::SOURCE)
        .as_any()
        .downcast_ref::<StringArray>();
    let target_col = graph
        .crate_edges
        .column(crate_edge_col::TARGET)
        .as_any()
        .downcast_ref::<StringArray>();
    let sk_col = graph
        .crate_edges
        .column(crate_edge_col::SOURCE_KIND)
        .as_any()
        .downcast_ref::<StringArray>();
    let dev_col = graph
        .crate_edges
        .column(crate_edge_col::DEV_DEP)
        .as_any()
        .downcast_ref::<BooleanArray>();

    let (Some(src), Some(tgt), Some(sk), Some(dev)) = (source_col, target_col, sk_col, dev_col)
    else {
        return;
    };

    for i in 0..src.len() {
        let kind = sk.value(i);
        if kind != "path" && kind != "workspace" {
            continue;
        }
        if dev.value(i) {
            continue;
        }
        let s = src.value(i).to_string();
        let t = tgt.value(i).to_string();
        if workspace_members.contains(&s) && workspace_members.contains(&t) {
            // Edge direction for Kahn's: dependency → dependent.
            // s (source in Cargo) depends on t (target), so t must build before s.
            // Add t → s so that when t is processed, s's in-degree decrements.
            adjacency.entry(t).or_default().push(s);
        }
    }
}

/// Generic Kahn's algorithm returning a flattened topological order.
fn kahn_sort(
    nodes: HashSet<String>,
    adjacency: HashMap<String, Vec<String>>,
    in_degree: HashMap<String, usize>,
) -> Result<Vec<String>, String> {
    let layers = kahn_layers(nodes, adjacency, in_degree)?;
    Ok(layers.into_iter().flatten().collect())
}

/// Generic Kahn's algorithm returning parallel layers.
fn kahn_layers(
    nodes: HashSet<String>,
    adjacency: HashMap<String, Vec<String>>,
    in_degree: HashMap<String, usize>,
) -> Result<Vec<Vec<String>>, String> {
    let mut in_deg = in_degree;
    let mut adj = adjacency;

    let mut queue: VecDeque<String> = nodes
        .iter()
        .filter(|n| in_deg.get(*n).copied().unwrap_or(0) == 0)
        .cloned()
        .collect();
    // Sort for determinism
    let mut queue_vec: Vec<String> = queue.drain(..).collect();
    queue_vec.sort();
    let mut queue: VecDeque<String> = queue_vec.into();

    let mut layers: Vec<Vec<String>> = Vec::new();
    let mut visited = 0usize;

    while !queue.is_empty() {
        let mut current_layer: Vec<String> = queue.drain(..).collect();
        current_layer.sort();
        visited += current_layer.len();

        let mut next_layer_candidates: Vec<String> = Vec::new();
        for node in &current_layer {
            for nbr in adj.remove(node).unwrap_or_default() {
                let deg = in_deg.entry(nbr.clone()).or_insert(0);
                if *deg > 0 {
                    *deg -= 1;
                }
                if *deg == 0 {
                    next_layer_candidates.push(nbr);
                }
            }
        }
        next_layer_candidates.sort();
        // Dedup in case multiple sources feed same target in same layer
        next_layer_candidates.dedup();
        layers.push(current_layer);
        queue.extend(next_layer_candidates);
    }

    if visited < nodes.len() {
        return Err(format!(
            "cyclic dependency detected: {} nodes in cycle(s) — {} sorted of {}",
            nodes.len() - visited,
            visited,
            nodes.len()
        ));
    }

    Ok(layers)
}

/// (node_ids, adjacency, in_degree) triple returned by `build_function_graph`.
type FunctionGraph = (
    HashSet<String>,
    HashMap<String, Vec<String>>,
    HashMap<String, usize>,
);

/// Build the function-level graph for a given crate.
///
/// Returns (node_ids, adjacency, in_degree) ready for Kahn's algorithm.
fn build_function_graph(
    crate_name: &str,
    nodes: &RecordBatch,
    edges: &RecordBatch,
) -> Result<FunctionGraph, String> {
    let id_col = nodes
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CodeNode id column is not StringArray")?;

    let file_col = nodes
        .column(node_col::FILE_PATH)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CodeNode file_path column is not StringArray")?;

    // The file_path for crate nodes starts with `crates/<crate_name>/`
    let crate_prefix = format!("crates/{}/", crate_name);

    let mut node_ids: HashSet<String> = HashSet::new();
    let mut id_set: HashSet<String> = HashSet::new();
    for i in 0..id_col.len() {
        let fp = file_col.value(i);
        if fp.starts_with(&crate_prefix) {
            let id = id_col.value(i).to_string();
            node_ids.insert(id.clone());
            id_set.insert(id);
        }
    }

    // Build adjacency from "calls" and "uses" edges within the same crate
    let src_col = edges
        .column(edge_col::SOURCE_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CodeEdge source_id column is not StringArray")?;
    let tgt_col = edges
        .column(edge_col::TARGET_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CodeEdge target_id column is not StringArray")?;
    // Predicate column may be Dictionary<Int8, Utf8> (from build_code_edges_batch) or plain Utf8
    // (from raw ingest). Cast to Utf8 to handle both.
    let pred_array = arrow::compute::cast(
        edges.column(edge_col::PREDICATE),
        &arrow::datatypes::DataType::Utf8,
    )
    .map_err(|e| format!("Failed to cast predicate column to Utf8: {e}"))?;
    let pred_col = pred_array
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CodeEdge predicate column is not StringArray after cast")?;

    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    for id in &node_ids {
        adjacency.entry(id.clone()).or_default();
        in_degree.entry(id.clone()).or_insert(0);
    }

    for i in 0..src_col.len() {
        let pred = pred_col.value(i);
        // Only consider call-dependency edges for build ordering
        if pred != "calls" && pred != "uses" {
            continue;
        }
        let src = src_col.value(i).to_string();
        let tgt = tgt_col.value(i).to_string();
        if id_set.contains(&src) && id_set.contains(&tgt) && src != tgt {
            // src (caller) depends on tgt (callee).
            // Callee must come BEFORE caller in build order.
            // Add tgt → src so that once tgt is processed, src's in-degree decrements.
            adjacency.entry(tgt).or_default().push(src.clone());
            *in_degree.entry(src).or_insert(0) += 1;
        }
    }

    Ok((node_ids, adjacency, in_degree))
}

/// Helper used by `sort_crates_parallel` — builds layer structure from a
/// workspace member set by calling the provided edge-wiring closure.
fn build_parallel_layers(
    workspace_members: &HashSet<String>,
    wire: impl Fn(&mut HashMap<String, Vec<String>>),
) -> Result<Vec<Vec<String>>, String> {
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    let mut in_degree: HashMap<String, usize> = HashMap::new();

    for name in workspace_members {
        adjacency.entry(name.clone()).or_default();
        in_degree.entry(name.clone()).or_insert(0);
    }

    wire(&mut adjacency);

    // Recompute in_degree from adjacency (wire may have added edges)
    let mut in_deg: HashMap<String, usize> = HashMap::new();
    for name in workspace_members {
        in_deg.entry(name.clone()).or_insert(0);
    }
    for targets in adjacency.values() {
        for t in targets {
            if workspace_members.contains(t) {
                *in_deg.entry(t.clone()).or_insert(0) += 1;
            }
        }
    }

    kahn_layers(workspace_members.clone(), adjacency, in_deg)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crate_graph::build_crate_graph;
    use std::path::PathBuf;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn test_sort_crates_wraps_topo_sort() {
        let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");
        let order = sort_crates(&graph).expect("no cycles expected");
        assert!(!order.is_empty(), "should return at least one crate");
        // nusy-arrow-core is a foundational crate — must precede nusy-being
        let core_pos = order.iter().position(|n| n == "nusy-arrow-core");
        let being_pos = order.iter().position(|n| n == "nusy-being");
        if let (Some(c), Some(b)) = (core_pos, being_pos) {
            assert!(c < b, "nusy-arrow-core must come before nusy-being");
        }
    }

    #[test]
    fn test_sort_crates_parallel_layers_are_independent() {
        let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");
        let layers = sort_crates_parallel(&graph).expect("no cycles expected");

        assert!(!layers.is_empty(), "should have at least one layer");

        // All layers together cover exactly the workspace members (no extras, no missing)
        let total: usize = layers.iter().map(|l| l.len()).sum();
        assert!(total > 0, "at least some workspace crates returned");
        let flat: HashSet<String> = layers.iter().flatten().cloned().collect();
        assert_eq!(
            flat.len(),
            total,
            "no duplicates across layers: {} unique, {} total",
            flat.len(),
            total
        );

        // Independence check: for every workspace dependency edge dep→dependent,
        // dep must be in a strictly EARLIER layer than dependent.
        let layer_index: HashMap<String, usize> = layers
            .iter()
            .enumerate()
            .flat_map(|(i, layer)| layer.iter().map(move |name| (name.clone(), i)))
            .collect();

        use crate::crate_schema::crate_edge_col;
        use arrow::array::StringArray;
        let src_col = graph
            .crate_edges
            .column(crate_edge_col::SOURCE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("source column is StringArray");
        let tgt_col = graph
            .crate_edges
            .column(crate_edge_col::TARGET)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("target column is StringArray");
        let dev_col = graph
            .crate_edges
            .column(crate_edge_col::DEV_DEP)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("dev_dep column is BooleanArray");

        // CrateEdge direction: source = dependent (declares the dep), target = dependency (built first)
        // Skip dev-deps — they don't affect compilation order (e.g., test-only deps)
        for i in 0..src_col.len() {
            if dev_col.value(i) {
                continue; // dev-dependencies don't constrain build order
            }
            let dependent = src_col.value(i); // the crate that declares this dependency
            let dep = tgt_col.value(i); // the dependency (must build first)
            // Only check workspace→workspace edges
            let (Some(&dep_layer), Some(&dependent_layer)) =
                (layer_index.get(dep), layer_index.get(dependent))
            else {
                continue; // external or missing — skip
            };
            assert!(
                dep_layer < dependent_layer,
                "layer independence violated: '{dependent}' depends on '{dep}' but '{dep}' is in layer {dep_layer} and '{dependent}' is in layer {dependent_layer}; dependency must be in earlier layer"
            );
        }
    }

    #[test]
    fn test_sort_crates_parallel_nusy_arrow_core_in_first_layers() {
        let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");
        let layers = sort_crates_parallel(&graph).expect("no cycles");

        // nusy-arrow-core has no workspace dependencies — must appear in layer 0 or 1
        let core_layer = layers
            .iter()
            .position(|l| l.contains(&"nusy-arrow-core".to_string()));
        assert!(
            core_layer.is_some(),
            "nusy-arrow-core must appear in parallel layers"
        );
        assert!(
            core_layer.unwrap() <= 1,
            "nusy-arrow-core is a root crate, should be in layer 0 or 1 (got {})",
            core_layer.unwrap()
        );
    }

    #[test]
    fn test_sort_crates_parallel_count_matches_flat_sort() {
        let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");
        let flat = sort_crates(&graph).expect("no cycles");
        let layers = sort_crates_parallel(&graph).expect("no cycles");
        let parallel_total: usize = layers.iter().map(|l| l.len()).sum();
        assert_eq!(
            flat.len(),
            parallel_total,
            "flat sort and parallel sort must cover the same crates"
        );
    }

    #[test]
    fn test_detects_cyclic_dependency() {
        use crate::crate_schema::{crate_edge_schema, crate_node_schema};
        use arrow::array::{BooleanArray, RecordBatch, StringArray};
        use std::sync::Arc;

        // Build a synthetic graph with cycle: A → B → A
        let nodes = RecordBatch::try_new(
            crate_node_schema(),
            vec![
                Arc::new(StringArray::from(vec!["crate-a", "crate-b"])),
                Arc::new(StringArray::from(vec!["0.1.0", "0.1.0"])),
                Arc::new(BooleanArray::from(vec![true, true])),
                Arc::new(StringArray::from(vec![None::<&str>, None])),
                Arc::new(StringArray::from(vec!["2021", "2021"])),
            ],
        )
        .expect("build test nodes");

        let edges = RecordBatch::try_new(
            crate_edge_schema(),
            vec![
                Arc::new(StringArray::from(vec!["crate-a", "crate-b"])),
                Arc::new(StringArray::from(vec!["crate-b", "crate-a"])),
                Arc::new(StringArray::from(vec!["*", "*"])),
                Arc::new(BooleanArray::from(vec![false, false])),
                Arc::new(BooleanArray::from(vec![false, false])),
                Arc::new(BooleanArray::from(vec![false, false])),
                Arc::new(StringArray::from(vec!["path", "path"])),
            ],
        )
        .expect("build test edges");

        let graph = CrateGraph {
            crate_nodes: nodes,
            crate_edges: edges,
        };

        let result = sort_crates(&graph);
        assert!(result.is_err(), "cycle should be detected");
        let err = result.unwrap_err();
        assert!(
            err.contains("cycle") || err.contains("cyclic"),
            "error message should mention cycle: {err}"
        );
    }

    #[test]
    fn test_parallelism_stats() {
        let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");
        let stats = crate_parallelism_stats(&graph).expect("stats should succeed");

        // NuSy workspace expected characteristics:
        assert!(
            stats.layer_count >= 3,
            "expected >= 3 layers, got {}",
            stats.layer_count
        );
        assert!(
            stats.max_layer_width >= 2,
            "expected max_layer_width >= 2, got {}",
            stats.max_layer_width
        );
        assert!(
            !stats.critical_path.is_empty(),
            "critical path should not be empty"
        );
        assert!(
            stats.critical_path.len() >= 2,
            "critical path should span at least 2 crates"
        );
    }

    // ─── Function-level sort tests ────────────────────────────────────────────

    #[test]
    fn test_sort_functions_callee_before_caller() {
        use crate::schema::{CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind};
        use crate::schema::{build_code_edges_batch, build_code_nodes_batch};

        // A calls B → B (callee) must appear BEFORE A (caller) in build order
        let nodes = build_code_nodes_batch(&[
            CodeNode {
                id: "fn_a".to_string(),
                kind: CodeNodeKind::Function,
                name: "a".to_string(),
                file_path: Some("crates/tc/src/lib.rs".to_string()),
                ..Default::default()
            },
            CodeNode {
                id: "fn_b".to_string(),
                kind: CodeNodeKind::Function,
                name: "b".to_string(),
                file_path: Some("crates/tc/src/lib.rs".to_string()),
                ..Default::default()
            },
        ])
        .expect("build nodes");

        // Edge: fn_a (caller/source) calls fn_b (callee/target)
        let edges = build_code_edges_batch(&[CodeEdge {
            source_id: "fn_a".to_string(),
            target_id: "fn_b".to_string(),
            predicate: CodeEdgePredicate::Calls,
            weight: None,
            commit_id: None,
        }])
        .expect("build edges");

        let order = sort_functions_in_crate("tc", &nodes, &edges).expect("no cycle");

        assert!(
            order.contains(&"fn_a".to_string()),
            "fn_a must be in sort output"
        );
        assert!(
            order.contains(&"fn_b".to_string()),
            "fn_b must be in sort output"
        );

        let pos_a = order.iter().position(|id| id == "fn_a").unwrap();
        let pos_b = order.iter().position(|id| id == "fn_b").unwrap();
        assert!(
            pos_b < pos_a,
            "callee fn_b (pos {pos_b}) must come before caller fn_a (pos {pos_a})"
        );
    }

    #[test]
    fn test_sort_functions_parallel() {
        use crate::schema::build_code_nodes_batch;
        use crate::schema::{CodeNode, CodeNodeKind, code_edges_schema};
        use arrow::array::RecordBatch;
        use std::sync::Arc;

        // Two independent functions in the same crate — no edges between them.
        // Both must appear in the output.
        let nodes = build_code_nodes_batch(&[
            CodeNode {
                id: "fn_x".to_string(),
                kind: CodeNodeKind::Function,
                name: "x".to_string(),
                file_path: Some("crates/ind/src/lib.rs".to_string()),
                ..Default::default()
            },
            CodeNode {
                id: "fn_y".to_string(),
                kind: CodeNodeKind::Function,
                name: "y".to_string(),
                file_path: Some("crates/ind/src/lib.rs".to_string()),
                ..Default::default()
            },
        ])
        .expect("build nodes");

        // Empty edges batch — no call relationships
        let edges = RecordBatch::new_empty(Arc::new(code_edges_schema()));

        let layers = sort_functions_parallel("ind", &nodes, &edges).expect("no cycle");

        let all_ids: Vec<String> = layers.iter().flatten().cloned().collect();
        assert!(
            all_ids.contains(&"fn_x".to_string()),
            "fn_x must appear in parallel output"
        );
        assert!(
            all_ids.contains(&"fn_y".to_string()),
            "fn_y must appear in parallel output"
        );
        // Both are independent so they should land in the same (first) layer
        assert_eq!(
            layers.len(),
            1,
            "two independent functions should be in one layer"
        );
        assert_eq!(layers[0].len(), 2, "both functions in layer 0");
    }

    #[test]
    fn test_sort_functions_parallel_callee_in_earlier_layer() {
        use crate::schema::{CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind};
        use crate::schema::{build_code_edges_batch, build_code_nodes_batch};

        // Call chain: root → helper → leaf
        // Expected layers: [leaf], [helper], [root]
        let nodes = build_code_nodes_batch(&[
            CodeNode {
                id: "fn_root".to_string(),
                kind: CodeNodeKind::Function,
                name: "root".to_string(),
                file_path: Some("crates/tc2/src/lib.rs".to_string()),
                ..Default::default()
            },
            CodeNode {
                id: "fn_helper".to_string(),
                kind: CodeNodeKind::Function,
                name: "helper".to_string(),
                file_path: Some("crates/tc2/src/lib.rs".to_string()),
                ..Default::default()
            },
            CodeNode {
                id: "fn_leaf".to_string(),
                kind: CodeNodeKind::Function,
                name: "leaf".to_string(),
                file_path: Some("crates/tc2/src/lib.rs".to_string()),
                ..Default::default()
            },
        ])
        .expect("build nodes");

        let edges = build_code_edges_batch(&[
            CodeEdge {
                source_id: "fn_root".to_string(),
                target_id: "fn_helper".to_string(),
                predicate: CodeEdgePredicate::Calls,
                weight: None,
                commit_id: None,
            },
            CodeEdge {
                source_id: "fn_helper".to_string(),
                target_id: "fn_leaf".to_string(),
                predicate: CodeEdgePredicate::Calls,
                weight: None,
                commit_id: None,
            },
        ])
        .expect("build edges");

        let layers = sort_functions_parallel("tc2", &nodes, &edges).expect("no cycle");

        let layer_index: HashMap<String, usize> = layers
            .iter()
            .enumerate()
            .flat_map(|(i, layer)| layer.iter().map(move |id| (id.clone(), i)))
            .collect();

        let root_layer = *layer_index.get("fn_root").expect("fn_root in layers");
        let helper_layer = *layer_index.get("fn_helper").expect("fn_helper in layers");
        let leaf_layer = *layer_index.get("fn_leaf").expect("fn_leaf in layers");

        assert!(
            leaf_layer < helper_layer,
            "leaf (layer {leaf_layer}) must be in earlier layer than helper (layer {helper_layer})"
        );
        assert!(
            helper_layer < root_layer,
            "helper (layer {helper_layer}) must be in earlier layer than root (layer {root_layer})"
        );
    }
}
