//! Build a crate-level dependency graph from a Cargo workspace.
//!
//! [`build_crate_graph`] walks `crates/*/Cargo.toml`, parses each manifest, and
//! produces two Arrow [`RecordBatch`] tables:
//!
//! - **CrateNode** — one row per workspace member crate
//! - **CrateEdge** — one row per dependency relationship
//!
//! [`topo_sort_crates`] runs Kahn's algorithm over the workspace-internal edges and
//! returns crate names in dependency-first (build) order.
//!
//! # Example
//!
//! ```no_run
//! use nusy_codegraph::crate_graph::build_crate_graph;
//! use std::path::Path;
//!
//! let graph = build_crate_graph(Path::new(".")).unwrap();
//! println!("{} crates, {} edges",
//!     graph.crate_nodes.num_rows(),
//!     graph.crate_edges.num_rows());
//! ```

use crate::cargo_parser::{CrateManifest, DependencySource, parse_workspace_dependencies};
use crate::crate_schema::{crate_edge_schema, crate_node_schema};
use arrow::array::{BooleanArray, RecordBatch, StringArray};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The two Arrow tables that describe a workspace's crate dependency graph.
pub struct CrateGraph {
    /// CrateNode RecordBatch — one row per workspace member.
    pub crate_nodes: RecordBatch,
    /// CrateEdge RecordBatch — one row per dependency relationship.
    pub crate_edges: RecordBatch,
}

// ─── build_crate_graph ───────────────────────────────────────────────────────

/// Build a [`CrateGraph`] by scanning all `Cargo.toml` files under `workspace_root`.
///
/// Algorithm:
/// 1. Parse the workspace root `Cargo.toml` to get shared dep versions.
/// 2. Glob `crates/*/Cargo.toml` for workspace members.
/// 3. Parse each member manifest.
/// 4. Build CrateNode records for workspace members.
/// 5. Build CrateEdge records for all dependencies (runtime, dev, build).
pub fn build_crate_graph(workspace_root: &Path) -> Result<CrateGraph, String> {
    // ── Step 1: workspace-level dep versions ─────────────────────────────────
    let ws_toml = workspace_root.join("Cargo.toml");
    let ws_deps = parse_workspace_dependencies(&ws_toml).unwrap_or_default();

    // Also store workspace package version under sentinel "" key so version.workspace = true
    // in [package] can be resolved.
    let ws_pkg_version =
        read_workspace_package_version(&ws_toml).unwrap_or_else(|| "0.0.0".to_string());
    let mut ws_deps = ws_deps;
    ws_deps.insert(String::new(), ws_pkg_version.clone());

    // ── Step 2: find workspace member Cargo.toml files ───────────────────────
    let member_tomls = find_member_cargo_tomls(workspace_root)?;
    let member_names: HashSet<String> = member_tomls
        .iter()
        .filter_map(|p| {
            // Peek at name without full parse
            quick_read_crate_name(p)
        })
        .collect();

    // ── Step 3: parse each member ─────────────────────────────────────────────
    let mut manifests: Vec<CrateManifest> = Vec::with_capacity(member_tomls.len());
    for toml_path in &member_tomls {
        match CrateManifest::from_path_with_workspace(toml_path, &ws_deps, true) {
            Ok(m) => manifests.push(m),
            Err(e) => {
                // Log but continue — a single bad Cargo.toml shouldn't abort the whole build
                eprintln!("warn: skipping {}: {e}", toml_path.display());
            }
        }
    }

    // ── Step 4: build CrateNode rows ─────────────────────────────────────────
    let mut node_ids: Vec<&str> = Vec::with_capacity(manifests.len());
    let mut node_versions: Vec<&str> = Vec::with_capacity(manifests.len());
    let mut node_workspace_members: Vec<bool> = Vec::with_capacity(manifests.len());
    let mut node_descriptions: Vec<Option<&str>> = Vec::with_capacity(manifests.len());
    let mut node_editions: Vec<&str> = Vec::with_capacity(manifests.len());

    for m in &manifests {
        node_ids.push(m.name.as_str());
        node_versions.push(m.version.as_str());
        node_workspace_members.push(m.workspace_member);
        node_descriptions.push(m.description.as_deref());
        node_editions.push(m.edition.as_str());
    }

    let crate_nodes = RecordBatch::try_new(
        crate_node_schema(),
        vec![
            Arc::new(StringArray::from(node_ids)),
            Arc::new(StringArray::from(node_versions)),
            Arc::new(BooleanArray::from(node_workspace_members)),
            Arc::new(StringArray::from(node_descriptions)),
            Arc::new(StringArray::from(node_editions)),
        ],
    )
    .map_err(|e| format!("Arrow error building CrateNodes: {e}"))?;

    // ── Step 5: build CrateEdge rows ─────────────────────────────────────────
    let mut edge_sources: Vec<String> = Vec::new();
    let mut edge_targets: Vec<String> = Vec::new();
    let mut edge_version_reqs: Vec<String> = Vec::new();
    let mut edge_optionals: Vec<bool> = Vec::new();
    let mut edge_dev_deps: Vec<bool> = Vec::new();
    let mut edge_build_deps: Vec<bool> = Vec::new();
    let mut edge_source_kinds: Vec<String> = Vec::new();

    for m in &manifests {
        for dep in m.all_dependencies() {
            // Only include deps that are workspace members OR external (include all)
            // We include ALL to give the full picture; topo_sort only uses workspace edges
            let source_kind = match &dep.source {
                DependencySource::Workspace => "workspace",
                DependencySource::CratesIo => "crates_io",
                DependencySource::Git { .. } => "git",
                DependencySource::Path { .. } => "path",
            };

            // For path/workspace deps pointing to internal crates, use the canonical name
            let target_name = resolve_dep_name(dep, &member_names, workspace_root);

            edge_sources.push(m.name.clone());
            edge_targets.push(target_name);
            edge_version_reqs.push(dep.version_req.clone());
            edge_optionals.push(dep.optional);
            edge_dev_deps.push(dep.dev);
            edge_build_deps.push(dep.build);
            edge_source_kinds.push(source_kind.to_string());
        }
    }

    let edge_source_refs: Vec<&str> = edge_sources.iter().map(|s| s.as_str()).collect();
    let edge_target_refs: Vec<&str> = edge_targets.iter().map(|s| s.as_str()).collect();
    let edge_version_req_refs: Vec<&str> = edge_version_reqs.iter().map(|s| s.as_str()).collect();
    let edge_source_kind_refs: Vec<&str> = edge_source_kinds.iter().map(|s| s.as_str()).collect();

    let crate_edges = RecordBatch::try_new(
        crate_edge_schema(),
        vec![
            Arc::new(StringArray::from(edge_source_refs)),
            Arc::new(StringArray::from(edge_target_refs)),
            Arc::new(StringArray::from(edge_version_req_refs)),
            Arc::new(BooleanArray::from(edge_optionals)),
            Arc::new(BooleanArray::from(edge_dev_deps)),
            Arc::new(BooleanArray::from(edge_build_deps)),
            Arc::new(StringArray::from(edge_source_kind_refs)),
        ],
    )
    .map_err(|e| format!("Arrow error building CrateEdges: {e}"))?;

    // ── Step 6: add external crates to CrateNode (workspace_member = false) ──
    // Collect unique external targets (those not in the workspace) from edge_targets.
    let workspace_names: HashSet<&str> = manifests.iter().map(|m| m.name.as_str()).collect();

    // Map external name → best version_req seen (first occurrence wins)
    let mut external_versions: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (i, target) in edge_targets.iter().enumerate() {
        if !workspace_names.contains(target.as_str()) {
            external_versions
                .entry(target.clone())
                .or_insert_with(|| edge_version_reqs[i].clone());
        }
    }

    if !external_versions.is_empty() {
        // Append external rows to a new CrateNode batch that extends the workspace rows.
        let mut all_ids: Vec<String> = manifests.iter().map(|m| m.name.clone()).collect();
        let mut all_versions: Vec<String> = manifests.iter().map(|m| m.version.clone()).collect();
        let mut all_ws: Vec<bool> = manifests.iter().map(|_| true).collect();
        let mut all_desc: Vec<Option<String>> =
            manifests.iter().map(|m| m.description.clone()).collect();
        let mut all_ed: Vec<String> = manifests.iter().map(|m| m.edition.clone()).collect();

        for (name, ver) in &external_versions {
            all_ids.push(name.clone());
            all_versions.push(ver.clone());
            all_ws.push(false);
            all_desc.push(None);
            all_ed.push(String::new());
        }

        let id_refs: Vec<&str> = all_ids.iter().map(|s| s.as_str()).collect();
        let ver_refs: Vec<&str> = all_versions.iter().map(|s| s.as_str()).collect();
        let desc_refs: Vec<Option<&str>> = all_desc.iter().map(|o| o.as_deref()).collect();
        let ed_refs: Vec<&str> = all_ed.iter().map(|s| s.as_str()).collect();

        let crate_nodes = RecordBatch::try_new(
            crate_node_schema(),
            vec![
                Arc::new(StringArray::from(id_refs)),
                Arc::new(StringArray::from(ver_refs)),
                Arc::new(BooleanArray::from(all_ws)),
                Arc::new(StringArray::from(desc_refs)),
                Arc::new(StringArray::from(ed_refs)),
            ],
        )
        .map_err(|e| format!("Arrow error building CrateNodes (with external): {e}"))?;

        return Ok(CrateGraph {
            crate_nodes,
            crate_edges,
        });
    }

    Ok(CrateGraph {
        crate_nodes,
        crate_edges,
    })
}

// ─── topo_sort_crates ────────────────────────────────────────────────────────

/// Topologically sort workspace crate names using Kahn's algorithm.
///
/// Only workspace-internal edges (path/workspace source kind) are used — external
/// crates.io deps are ignored for ordering.  The result is in dependency-first
/// order: if crate A depends on crate B, B appears before A.
///
/// Returns `Err` if a cycle is detected (should not happen in a valid workspace).
pub fn topo_sort_crates(graph: &CrateGraph) -> Result<Vec<String>, String> {
    use crate::crate_schema::crate_edge_col;
    use arrow::array::Array;
    use arrow::array::{BooleanArray, StringArray};

    // Collect workspace member names from CrateNode table (workspace_member = true only)
    use crate::crate_schema::crate_node_col;

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

    // Build adjacency map for workspace-internal edges only
    // adjacency[source] = set of targets
    let mut adjacency: HashMap<String, HashSet<String>> = HashMap::new();
    // in_degree[crate] = number of workspace crates that depend on it
    let mut in_degree: HashMap<String, usize> = HashMap::new();

    // Initialize all workspace members
    for name in &workspace_members {
        adjacency.entry(name.clone()).or_default();
        in_degree.entry(name.clone()).or_insert(0);
    }

    let source_col = graph
        .crate_edges
        .column(crate_edge_col::SOURCE)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CrateEdge source column is not StringArray")?;
    let target_col = graph
        .crate_edges
        .column(crate_edge_col::TARGET)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CrateEdge target column is not StringArray")?;
    let source_kind_col = graph
        .crate_edges
        .column(crate_edge_col::SOURCE_KIND)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or("CrateEdge source_kind column is not StringArray")?;
    let dev_dep_col = graph
        .crate_edges
        .column(crate_edge_col::DEV_DEP)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or("CrateEdge dev_dep column is not BooleanArray")?;

    for i in 0..source_col.len() {
        let source_kind = source_kind_col.value(i);
        // Only use workspace-internal edges (path or workspace source kind)
        if source_kind != "path" && source_kind != "workspace" {
            continue;
        }
        // Skip dev deps for build order (they don't affect compilation order)
        if dev_dep_col.value(i) {
            continue;
        }

        let src = source_col.value(i).to_string();
        let tgt = target_col.value(i).to_string();

        // Only include edges where both ends are workspace members
        if !workspace_members.contains(&src) || !workspace_members.contains(&tgt) {
            continue;
        }

        // src depends on tgt → tgt must come before src
        // Kahn's: edge is tgt → src in the "comes before" sense
        // We build: adjacency[tgt].insert(src), in_degree[src] += 1
        if adjacency
            .entry(tgt.clone())
            .or_default()
            .insert(src.clone())
        {
            *in_degree.entry(src).or_insert(0) += 1;
        }
    }

    // Kahn's algorithm
    let mut queue: VecDeque<String> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(name, _)| name.clone())
        .collect();

    // Sort for determinism
    let mut sorted_queue: Vec<String> = queue.drain(..).collect();
    sorted_queue.sort();
    let mut queue: VecDeque<String> = sorted_queue.into_iter().collect();

    let mut result: Vec<String> = Vec::with_capacity(workspace_members.len());

    while let Some(node) = queue.pop_front() {
        result.push(node.clone());

        if let Some(successors) = adjacency.get(&node) {
            let mut succ_sorted: Vec<String> = successors.iter().cloned().collect();
            succ_sorted.sort();
            for succ in succ_sorted {
                let deg = in_degree.entry(succ.clone()).or_insert(0);
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(succ);
                }
            }
        }
    }

    if result.len() != workspace_members.len() {
        return Err(format!(
            "cycle detected in workspace dependency graph: processed {} of {} crates",
            result.len(),
            workspace_members.len()
        ));
    }

    Ok(result)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Find all `Cargo.toml` files for workspace members by walking `crates/*/Cargo.toml`.
///
/// Falls back to scanning any `*/Cargo.toml` one level below `workspace_root` if the
/// `crates/` subdirectory does not exist.
fn find_member_cargo_tomls(workspace_root: &Path) -> Result<Vec<PathBuf>, String> {
    let crates_dir = workspace_root.join("crates");

    let search_dir = if crates_dir.is_dir() {
        crates_dir
    } else {
        workspace_root.to_path_buf()
    };

    let mut tomls: Vec<PathBuf> = Vec::new();

    let entries = std::fs::read_dir(&search_dir)
        .map_err(|e| format!("cannot read {}: {e}", search_dir.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let toml = path.join("Cargo.toml");
            if toml.exists() {
                tomls.push(toml);
            }
        }
    }

    tomls.sort();
    Ok(tomls)
}

/// Quickly read the `[package].name` from a Cargo.toml without full parsing.
fn quick_read_crate_name(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let doc: toml::Value = raw.parse().ok()?;
    doc.get("package")?
        .get("name")?
        .as_str()
        .map(|s| s.to_string())
}

/// Read `[workspace.package].version` from the workspace root Cargo.toml.
fn read_workspace_package_version(ws_toml: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(ws_toml).ok()?;
    let doc: toml::Value = raw.parse().ok()?;
    doc.get("workspace")?
        .get("package")?
        .get("version")?
        .as_str()
        .map(|s| s.to_string())
}

/// Resolve the target crate name for a dependency.
///
/// For path dependencies (`../nusy-arrow-core`), the actual crate name
/// may differ from the key in Cargo.toml. We try to read the name from
/// the target's Cargo.toml. Falls back to the dep's recorded `name` field.
fn resolve_dep_name(
    dep: &crate::cargo_parser::CrateDependency,
    _member_names: &HashSet<String>,
    workspace_root: &Path,
) -> String {
    if let DependencySource::Path { .. } = &dep.source {
        // Path is relative to the declaring crate, but we don't have that context here.
        // We look for <workspace_root>/crates/<dep.name>/Cargo.toml as a heuristic.
        let candidate = workspace_root
            .join("crates")
            .join(&dep.name)
            .join("Cargo.toml");
        if let Some(actual_name) = quick_read_crate_name(&candidate) {
            return actual_name;
        }
    }
    dep.name.clone()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crate_schema::{crate_edge_col, crate_node_col};
    use arrow::array::Array;
    use arrow::array::{BooleanArray, StringArray};

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    // ── Test 1: build_crate_graph succeeds on real workspace ─────────────────

    #[test]
    fn test_build_crate_graph_on_workspace() {
        let graph = build_crate_graph(&workspace_root())
            .expect("build_crate_graph should succeed on NuSy workspace");

        // Should have at least several crates
        assert!(
            graph.crate_nodes.num_rows() >= 5,
            "expected >= 5 crate nodes, got {}",
            graph.crate_nodes.num_rows()
        );
        // Should have edges
        assert!(graph.crate_edges.num_rows() > 0, "expected > 0 crate edges");
        // Schema column counts
        assert_eq!(graph.crate_nodes.num_columns(), 5);
        assert_eq!(graph.crate_edges.num_columns(), 7);
    }

    // ── Test 2: nusy-arrow-core is a workspace member ─────────────────────────

    #[test]
    fn test_nusy_arrow_core_is_workspace_member() {
        let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

        let id_col = graph
            .crate_nodes
            .column(crate_node_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let wm_col = graph
            .crate_nodes
            .column(crate_node_col::WORKSPACE_MEMBER)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();

        let row = (0..id_col.len())
            .find(|&i| id_col.value(i) == "nusy-arrow-core")
            .expect("nusy-arrow-core should be in CrateNode table");

        assert!(
            wm_col.value(row),
            "nusy-arrow-core should have workspace_member=true"
        );
    }

    // ── Test 3: nusy-codegraph depends on nusy-arrow-core ─────────────────────

    #[test]
    fn test_nusy_codegraph_depends_on_nusy_arrow_core() {
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

        let has_edge = (0..source_col.len()).any(|i| {
            source_col.value(i) == "nusy-codegraph" && target_col.value(i) == "nusy-arrow-core"
        });

        assert!(
            has_edge,
            "expected nusy-codegraph → nusy-arrow-core edge in CrateEdge table"
        );
    }

    // ── Test 4: topo_sort puts nusy-arrow-core before nusy-codegraph ──────────

    #[test]
    fn test_topo_sort_core_before_codegraph() {
        let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

        let order =
            topo_sort_crates(&graph).expect("topo_sort_crates should succeed with no cycles");

        let core_pos = order.iter().position(|n| n == "nusy-arrow-core");
        let codegraph_pos = order.iter().position(|n| n == "nusy-codegraph");

        let core_pos = core_pos.expect("nusy-arrow-core should be in topo sort result");
        let codegraph_pos = codegraph_pos.expect("nusy-codegraph should be in topo sort result");

        assert!(
            core_pos < codegraph_pos,
            "nusy-arrow-core (pos {core_pos}) should come before nusy-codegraph (pos {codegraph_pos}) in topo sort"
        );
    }

    // ── Test 5: no cycles in NuSy workspace ───────────────────────────────────

    #[test]
    fn test_no_cycles_in_nusy_workspace() {
        let graph = build_crate_graph(&workspace_root()).expect("build_crate_graph should succeed");

        let result = topo_sort_crates(&graph);
        assert!(
            result.is_ok(),
            "topo_sort_crates should not detect cycles: {:?}",
            result.err()
        );

        let order = result.unwrap();

        // topo_sort_crates returns only workspace members; count them separately
        // (graph.crate_nodes includes external crates added in EX-3171)
        use arrow::array::BooleanArray;
        let wm_col = graph
            .crate_nodes
            .column(crate::crate_schema::crate_node_col::WORKSPACE_MEMBER)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("workspace_member must be BooleanArray");
        let ws_count = (0..wm_col.len()).filter(|&i| wm_col.value(i)).count();

        assert_eq!(
            order.len(),
            ws_count,
            "topo sort should return all {ws_count} workspace crates (total rows incl. external: {})",
            graph.crate_nodes.num_rows()
        );
    }

    // ── Test 6: topo_sort detects cycle ───────────────────────────────────────

    #[test]
    fn test_topo_sort_detects_cycle() {
        use crate::crate_schema::{crate_edge_schema, crate_node_schema};

        // Build a minimal CrateGraph with a cycle: A → B → A
        let node_schema = crate_node_schema();
        let edge_schema = crate_edge_schema();

        let nodes = RecordBatch::try_new(
            node_schema,
            vec![
                Arc::new(StringArray::from(vec!["crate-a", "crate-b"])),
                Arc::new(StringArray::from(vec!["0.1.0", "0.1.0"])),
                Arc::new(BooleanArray::from(vec![true, true])),
                Arc::new(StringArray::from(vec![None::<&str>, None::<&str>])),
                Arc::new(StringArray::from(vec!["2024", "2024"])),
            ],
        )
        .unwrap();

        // A depends on B, B depends on A — cycle
        let edges = RecordBatch::try_new(
            edge_schema,
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
        .unwrap();

        let graph = CrateGraph {
            crate_nodes: nodes,
            crate_edges: edges,
        };

        let result = topo_sort_crates(&graph);
        assert!(
            result.is_err(),
            "topo_sort should return Err when a cycle exists"
        );
        assert!(
            result.unwrap_err().contains("cycle"),
            "error message should mention 'cycle'"
        );
    }
}
