//! Build orchestrator — graph-native workspace build from ingested code graph.
//!
//! EX-3181 Phase 1: Orchestrates a full workspace build by:
//! 1. Ingesting the workspace into a code graph (`ingest_workspace`)
//! 2. Building the crate dependency graph and computing topological order
//! 3. Compiling each crate's functions via `CachedWasmCompiler`
//! 4. Optionally discovering and running tests
//!
//! The build does not produce a native binary — it compiles all function bodies
//! to WASM modules in the sandbox. The "binary" is the set of compiled WASM
//! artifacts. The report shows what compiled and what did not.

use std::path::Path;
use std::time::Instant;

use nusy_codegraph::crate_graph::build_crate_graph;
use nusy_codegraph::ingest_pipeline::{WorkspaceIngestResult, ingest_workspace};
use nusy_codegraph::schema::{CodeNode, CodeNodeKind};
use nusy_codegraph::test_discovery::discover_tests;
use nusy_codegraph::topo_sort::sort_crates;

use crate::cached_compiler::{BuildReport, CachedWasmCompiler, build_workspace};
use crate::test_runner::{TestSuiteReport, run_all_tests};
use crate::wasm_compiler::WasmCompiler;

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for a workspace build.
#[derive(Debug, Clone, Default)]
pub struct BuildConfig {
    /// Stop on first test failure.
    pub fail_fast: bool,
    /// Run tests after compilation.
    pub run_tests: bool,
    /// Ignore cache (clean build).
    pub clean: bool,
    /// Build only one crate (by name).
    pub crate_filter: Option<String>,
    /// Load CodeNodes from a pre-ingested Parquet graph directory instead of
    /// re-ingesting the workspace. The path must point to a directory that
    /// contains `nodes.parquet` (produced by `write_graph_parquet()`).
    /// Falls back to live ingestion if the path does not exist.
    pub graph_path: Option<std::path::PathBuf>,
    /// Filter tests to those whose name contains this substring.
    /// When set, only tests whose `id` or `name` field contains the filter
    /// string are executed. Useful for targeted reruns after a code change.
    pub function_filter: Option<String>,
}

// ── Reports ──────────────────────────────────────────────────────────────────

/// Build report for a single crate.
#[derive(Debug)]
pub struct CrateBuildReport {
    /// Crate name.
    pub crate_name: String,
    /// Compilation report from `build_workspace`.
    pub build: BuildReport,
    /// Number of function nodes in this crate.
    pub function_count: usize,
}

/// Aggregate report for the entire workspace build.
#[derive(Debug)]
pub struct WorkspaceBuildReport {
    /// Per-crate build reports (in topological order).
    pub crate_reports: Vec<CrateBuildReport>,
    /// Test suite reports (empty if tests were not run).
    pub test_reports: Vec<TestSuiteReport>,
    /// Total function nodes across all crates.
    pub total_functions: usize,
    /// Functions successfully compiled (cache hit or fresh compile).
    pub compiled: usize,
    /// Functions served from cache.
    pub cached: usize,
    /// Functions that failed to compile.
    pub compile_errors: usize,
    /// Total build duration in milliseconds.
    pub total_duration_ms: u64,
}

impl WorkspaceBuildReport {
    /// Format as a human-readable build summary.
    pub fn format(&self) -> String {
        let mut lines = Vec::new();
        lines.push("=== graph.build() Workspace Report ===".to_string());
        lines.push(format!("Crates built: {}", self.crate_reports.len()));
        lines.push(format!("Total functions: {}", self.total_functions));
        lines.push(format!(
            "Compiled: {} (cached: {}, fresh: {})",
            self.compiled,
            self.cached,
            self.compiled.saturating_sub(self.cached)
        ));
        if self.compile_errors > 0 {
            lines.push(format!("Compile errors: {}", self.compile_errors));
        }
        lines.push(format!("Duration: {}ms", self.total_duration_ms));

        // Per-crate breakdown
        lines.push(String::new());
        for report in &self.crate_reports {
            let errors = report.build.errors.len();
            let error_tag = if errors > 0 {
                format!(" ({errors} errors)")
            } else {
                String::new()
            };
            lines.push(format!(
                "  {} — {} fns, {} cached, {} fresh{}",
                report.crate_name,
                report.function_count,
                report.build.cache_hits,
                report.build.cache_misses,
                error_tag,
            ));
        }

        // Test summary
        if !self.test_reports.is_empty() {
            lines.push(String::new());
            lines.push("--- Tests ---".to_string());
            let mut total_passed = 0;
            let mut total_failed = 0;
            let mut total_skipped = 0;
            for tr in &self.test_reports {
                total_passed += tr.passed;
                total_failed += tr.failed.len();
                total_skipped += tr.skipped;
            }
            lines.push(format!(
                "Tests: {} passed, {} failed, {} skipped across {} crate(s)",
                total_passed,
                total_failed,
                total_skipped,
                self.test_reports.len(),
            ));
        }

        lines.join("\n")
    }
}

// ── Orchestrator ─────────────────────────────────────────────────────────────

/// Build orchestrator — drives a full workspace build from graph data.
pub struct BuildOrchestrator {
    compiler: CachedWasmCompiler,
}

impl BuildOrchestrator {
    /// Create a new orchestrator with a fresh compiler and cache.
    pub fn new() -> Result<Self, String> {
        let wasm = WasmCompiler::new().map_err(|e| format!("WasmCompiler init: {e}"))?;
        Ok(Self {
            compiler: CachedWasmCompiler::new(wasm),
        })
    }

    /// Create an orchestrator with a pre-configured cached compiler.
    pub fn with_compiler(compiler: CachedWasmCompiler) -> Self {
        Self { compiler }
    }

    /// Build the workspace at the given root path.
    ///
    /// 1. Ingests the workspace into a code graph (or loads from `config.graph_path`)
    /// 2. Builds crate dependency graph and topological order
    /// 3. For each crate: extracts CodeNodes, compiles via `build_workspace()`
    /// 4. If `config.run_tests`: discovers and runs tests
    /// 5. Returns `WorkspaceBuildReport`
    pub fn build(
        &mut self,
        workspace_root: &Path,
        config: &BuildConfig,
    ) -> Result<WorkspaceBuildReport, String> {
        let ingest_result = if let Some(ref gp) = config.graph_path {
            if gp.join("nodes.parquet").exists() {
                eprintln!("nk build: loading code graph from {} …", gp.display());
                nusy_codegraph::ingest_pipeline::load_nodes_from_parquet(gp)?
            } else {
                eprintln!(
                    "nk build: graph path {} not found, falling back to live ingest …",
                    gp.display()
                );
                ingest_workspace(workspace_root)
            }
        } else {
            ingest_workspace(workspace_root)
        };
        self.build_from_graph(workspace_root, &ingest_result, config)
    }

    /// Build from pre-ingested graph data.
    ///
    /// Use this when the caller has already run `ingest_workspace()` and wants
    /// to avoid re-ingesting (e.g., during iterative development).
    pub fn build_from_graph(
        &mut self,
        workspace_root: &Path,
        ingest_result: &WorkspaceIngestResult,
        config: &BuildConfig,
    ) -> Result<WorkspaceBuildReport, String> {
        let overall_start = Instant::now();

        // ── Step 1: Get topological build order ──────────────────────────────
        let crate_graph =
            build_crate_graph(workspace_root).map_err(|e| format!("build_crate_graph: {e}"))?;
        let crate_order = sort_crates(&crate_graph).map_err(|e| format!("sort_crates: {e}"))?;

        // ── Step 2: Collect all CodeNodes from all crates ────────────────────
        let all_nodes: Vec<CodeNode> = ingest_result
            .crates
            .values()
            .flat_map(|r| r.nodes.iter().cloned())
            .collect();

        // ── Step 3: Build each crate in topological order ────────────────────
        let mut crate_reports = Vec::new();
        let mut total_functions = 0usize;
        let mut total_compiled = 0usize;
        let mut total_cached = 0u64;
        let mut total_errors = 0usize;

        // Determine which crates to build
        let crates_to_build: Vec<&str> = match &config.crate_filter {
            Some(filter) => {
                if crate_order.iter().any(|c| c == filter) {
                    vec![filter.as_str()]
                } else {
                    return Err(format!(
                        "crate '{}' not found in workspace (available: {})",
                        filter,
                        crate_order.join(", ")
                    ));
                }
            }
            None => crate_order.iter().map(|s| s.as_str()).collect(),
        };

        for crate_name in &crates_to_build {
            let crate_nodes = filter_nodes_for_crate(&all_nodes, crate_name);
            let fn_count = crate_nodes.len();

            let report = build_workspace(&crate_nodes, &mut self.compiler);

            total_functions += fn_count;
            total_compiled += (report.cache_hits + report.cache_misses) as usize;
            total_cached += report.cache_hits;
            total_errors += report.errors.len();

            crate_reports.push(CrateBuildReport {
                crate_name: crate_name.to_string(),
                build: report,
                function_count: fn_count,
            });
        }

        // ── Step 4: Optionally run tests ─────────────────────────────────────
        let test_reports = if config.run_tests {
            let nodes_batch = ingest_result
                .merged_nodes_batch()
                .map_err(|e| format!("merged_nodes_batch: {e}"))?;
            let tests = discover_tests(&[nodes_batch]);

            // Filter tests by crate if requested
            let crate_filtered = match &config.crate_filter {
                Some(filter) => {
                    let mut filtered = std::collections::HashMap::new();
                    if let Some(nodes) = tests.get(filter.as_str()) {
                        filtered.insert(filter.clone(), nodes.clone());
                    }
                    filtered
                }
                None => tests,
            };

            // Further filter by function name substring if --function was given
            let filtered_tests = if let Some(ref fn_filter) = config.function_filter {
                crate_filtered
                    .into_iter()
                    .map(|(crate_name, nodes)| {
                        let matching: Vec<_> = nodes
                            .into_iter()
                            .filter(|n| {
                                n.id.contains(fn_filter.as_str())
                                    || n.name.contains(fn_filter.as_str())
                            })
                            .collect();
                        (crate_name, matching)
                    })
                    .filter(|(_, nodes)| !nodes.is_empty())
                    .collect()
            } else {
                crate_filtered
            };

            run_all_tests(&mut self.compiler, &filtered_tests)
        } else {
            Vec::new()
        };

        let total_duration_ms = overall_start.elapsed().as_millis() as u64;

        Ok(WorkspaceBuildReport {
            crate_reports,
            test_reports,
            total_functions,
            compiled: total_compiled,
            cached: total_cached as usize,
            compile_errors: total_errors,
            total_duration_ms,
        })
    }

    /// Get the underlying compiler (for stats inspection).
    pub fn compiler(&self) -> &CachedWasmCompiler {
        &self.compiler
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Filter CodeNodes to those belonging to a specific crate.
///
/// A node belongs to crate `foo` if its `file_path` starts with `crates/foo/`.
/// Falls back to checking the `id` field for the crate prefix pattern.
fn filter_nodes_for_crate(all_nodes: &[CodeNode], crate_name: &str) -> Vec<CodeNode> {
    let crate_prefix = format!("crates/{}/", crate_name);

    all_nodes
        .iter()
        .filter(|node| {
            // Only include compilable function-like nodes
            if !is_compilable_kind(node.kind) {
                return false;
            }
            // Skip nodes without a body
            if node.body.is_none() {
                return false;
            }
            // Check file_path first (most reliable)
            if let Some(ref fp) = node.file_path {
                return fp.starts_with(&crate_prefix);
            }
            // Fallback: check node ID for crate path
            node.id.contains(&crate_prefix)
        })
        .cloned()
        .collect()
}

/// Whether a CodeNodeKind is compilable to WASM.
///
/// Only function-like nodes have bodies that can be compiled.
fn is_compilable_kind(kind: CodeNodeKind) -> bool {
    matches!(
        kind,
        CodeNodeKind::Function
            | CodeNodeKind::Method
            | CodeNodeKind::RustFn
            | CodeNodeKind::RustMethod
            | CodeNodeKind::RustTest
            | CodeNodeKind::PythonFunction
            | CodeNodeKind::PythonMethod
            | CodeNodeKind::PythonAsync
            | CodeNodeKind::PythonLambda
    )
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nusy_codegraph::schema::CodeNodeKind;

    // ── test_build_config_default ────────────────────────────────────────────

    #[test]
    fn test_build_config_default() {
        let config = BuildConfig::default();
        assert!(!config.fail_fast, "fail_fast should default to false");
        assert!(!config.run_tests, "run_tests should default to false");
        assert!(!config.clean, "clean should default to false");
        assert!(
            config.crate_filter.is_none(),
            "crate_filter should default to None"
        );
    }

    // ── test_build_small_graph ───────────────────────────────────────────────

    #[test]
    fn test_build_small_graph() {
        let mut orchestrator = BuildOrchestrator::new().expect("orchestrator init");

        // Build synthetic nodes that look like they belong to a crate
        let nodes = vec![
            CodeNode {
                id: "rust_fn:crates/test-crate/src/lib.rs::add".into(),
                kind: CodeNodeKind::RustFn,
                name: "add".into(),
                body: Some("pub fn add(a: i64, b: i64) -> i64 { a + b }".into()),
                file_path: Some("crates/test-crate/src/lib.rs".into()),
                ..CodeNode::default()
            },
            CodeNode {
                id: "rust_fn:crates/test-crate/src/lib.rs::mul".into(),
                kind: CodeNodeKind::RustFn,
                name: "mul".into(),
                body: Some("pub fn mul(a: i64, b: i64) -> i64 { a * b }".into()),
                file_path: Some("crates/test-crate/src/lib.rs".into()),
                ..CodeNode::default()
            },
        ];

        let crate_nodes = filter_nodes_for_crate(&nodes, "test-crate");
        assert_eq!(crate_nodes.len(), 2, "both nodes should match test-crate");

        let report = build_workspace(&crate_nodes, &mut orchestrator.compiler);
        assert_eq!(report.total, 2);
        assert_eq!(report.cache_misses, 2);
        assert!(report.errors.is_empty(), "simple functions should compile");
    }

    // ── test_build_filters_by_crate ──────────────────────────────────────────

    #[test]
    fn test_build_filters_by_crate() {
        let nodes = vec![
            CodeNode {
                id: "rust_fn:crates/alpha/src/lib.rs::fn_a".into(),
                kind: CodeNodeKind::RustFn,
                name: "fn_a".into(),
                body: Some("pub fn fn_a(x: i64) -> i64 { x }".into()),
                file_path: Some("crates/alpha/src/lib.rs".into()),
                ..CodeNode::default()
            },
            CodeNode {
                id: "rust_fn:crates/beta/src/lib.rs::fn_b".into(),
                kind: CodeNodeKind::RustFn,
                name: "fn_b".into(),
                body: Some("pub fn fn_b(x: i64) -> i64 { x + 1 }".into()),
                file_path: Some("crates/beta/src/lib.rs".into()),
                ..CodeNode::default()
            },
            CodeNode {
                id: "rust_fn:crates/alpha/src/lib.rs::fn_c".into(),
                kind: CodeNodeKind::RustFn,
                name: "fn_c".into(),
                body: Some("pub fn fn_c(x: i64) -> i64 { x + 2 }".into()),
                file_path: Some("crates/alpha/src/lib.rs".into()),
                ..CodeNode::default()
            },
        ];

        let alpha_nodes = filter_nodes_for_crate(&nodes, "alpha");
        assert_eq!(alpha_nodes.len(), 2, "alpha should have 2 nodes");
        assert!(
            alpha_nodes.iter().all(|n| n.name != "fn_b"),
            "fn_b belongs to beta, not alpha"
        );

        let beta_nodes = filter_nodes_for_crate(&nodes, "beta");
        assert_eq!(beta_nodes.len(), 1, "beta should have 1 node");
        assert_eq!(beta_nodes[0].name, "fn_b");
    }

    // ── test_report_format ───────────────────────────────────────────────────

    #[test]
    fn test_report_format() {
        let report = WorkspaceBuildReport {
            crate_reports: vec![CrateBuildReport {
                crate_name: "my-crate".to_string(),
                build: BuildReport {
                    total: 10,
                    cache_hits: 7,
                    cache_misses: 3,
                    elapsed: std::time::Duration::from_millis(500),
                    errors: vec![],
                },
                function_count: 10,
            }],
            test_reports: vec![],
            total_functions: 10,
            compiled: 10,
            cached: 7,
            compile_errors: 0,
            total_duration_ms: 500,
        };

        let formatted = report.format();
        assert!(
            formatted.contains("graph.build()"),
            "should contain graph.build() header"
        );
        assert!(
            formatted.contains("Crates built: 1"),
            "should show crate count"
        );
        assert!(
            formatted.contains("Total functions: 10"),
            "should show function count"
        );
        assert!(formatted.contains("cached: 7"), "should show cache hits");
        assert!(formatted.contains("fresh: 3"), "should show fresh compiles");
        assert!(formatted.contains("my-crate"), "should include crate name");
        assert!(
            formatted.contains("Duration: 500ms"),
            "should show duration"
        );
    }

    // ── test_incremental_build_uses_cache ────────────────────────────────────

    #[test]
    fn test_incremental_build_uses_cache() {
        let mut orchestrator = BuildOrchestrator::new().expect("orchestrator init");

        let nodes = vec![
            CodeNode {
                id: "rust_fn:crates/ic/src/lib.rs::fn0".into(),
                kind: CodeNodeKind::RustFn,
                name: "fn0".into(),
                body: Some("pub fn fn0(a: i64) -> i64 { a }".into()),
                file_path: Some("crates/ic/src/lib.rs".into()),
                ..CodeNode::default()
            },
            CodeNode {
                id: "rust_fn:crates/ic/src/lib.rs::fn1".into(),
                kind: CodeNodeKind::RustFn,
                name: "fn1".into(),
                body: Some("pub fn fn1(a: i64) -> i64 { a + 1 }".into()),
                file_path: Some("crates/ic/src/lib.rs".into()),
                ..CodeNode::default()
            },
            CodeNode {
                id: "rust_fn:crates/ic/src/lib.rs::fn2".into(),
                kind: CodeNodeKind::RustFn,
                name: "fn2".into(),
                body: Some("pub fn fn2(a: i64) -> i64 { a + 2 }".into()),
                file_path: Some("crates/ic/src/lib.rs".into()),
                ..CodeNode::default()
            },
        ];

        let crate_nodes = filter_nodes_for_crate(&nodes, "ic");

        // First build: all misses
        let r1 = build_workspace(&crate_nodes, &mut orchestrator.compiler);
        assert_eq!(r1.cache_misses, 3, "first build: all misses");
        assert_eq!(r1.cache_hits, 0, "first build: no hits");

        // Second build: all hits (same bodies)
        let r2 = build_workspace(&crate_nodes, &mut orchestrator.compiler);
        assert_eq!(r2.cache_hits, 3, "second build: all cache hits");
        assert_eq!(r2.cache_misses, 0, "second build: no misses");

        // Verify overall stats
        let stats = orchestrator.compiler().stats_snapshot();
        assert_eq!(stats.hits, 3);
        assert_eq!(stats.misses, 3);
        assert!((stats.hit_rate() - 0.5).abs() < 0.01);
    }

    // ── test_non_compilable_kinds_filtered ────────────────────────────────────

    #[test]
    fn test_non_compilable_kinds_filtered() {
        let nodes = vec![
            CodeNode {
                id: "rust_fn:crates/fc/src/lib.rs::good".into(),
                kind: CodeNodeKind::RustFn,
                name: "good".into(),
                body: Some("pub fn good(a: i64) -> i64 { a }".into()),
                file_path: Some("crates/fc/src/lib.rs".into()),
                ..CodeNode::default()
            },
            CodeNode {
                id: "rust_struct:crates/fc/src/lib.rs::MyStruct".into(),
                kind: CodeNodeKind::RustStruct,
                name: "MyStruct".into(),
                body: Some("pub struct MyStruct { x: i64 }".into()),
                file_path: Some("crates/fc/src/lib.rs".into()),
                ..CodeNode::default()
            },
            CodeNode {
                id: "rust_mod:crates/fc/src/lib.rs".into(),
                kind: CodeNodeKind::RustMod,
                name: "lib".into(),
                body: None,
                file_path: Some("crates/fc/src/lib.rs".into()),
                ..CodeNode::default()
            },
        ];

        let filtered = filter_nodes_for_crate(&nodes, "fc");
        assert_eq!(filtered.len(), 1, "only the RustFn should be compilable");
        assert_eq!(filtered[0].name, "good");
    }

    // ── test_is_compilable_kind ──────────────────────────────────────────────

    #[test]
    fn test_is_compilable_kind() {
        // Compilable kinds
        assert!(is_compilable_kind(CodeNodeKind::Function));
        assert!(is_compilable_kind(CodeNodeKind::Method));
        assert!(is_compilable_kind(CodeNodeKind::RustFn));
        assert!(is_compilable_kind(CodeNodeKind::RustMethod));
        assert!(is_compilable_kind(CodeNodeKind::RustTest));

        // Non-compilable kinds
        assert!(!is_compilable_kind(CodeNodeKind::File));
        assert!(!is_compilable_kind(CodeNodeKind::Module));
        assert!(!is_compilable_kind(CodeNodeKind::Class));
        assert!(!is_compilable_kind(CodeNodeKind::RustStruct));
        assert!(!is_compilable_kind(CodeNodeKind::RustEnum));
        assert!(!is_compilable_kind(CodeNodeKind::RustMod));
        assert!(!is_compilable_kind(CodeNodeKind::RustUse));
        assert!(!is_compilable_kind(CodeNodeKind::RustConst));
    }
}
