//! EX-3180 Phase 4: Parity check — graph-native test discovery vs cargo test.
//!
//! Ingests the NuSy workspace into a code graph and compares the test functions
//! discovered by `discover_tests()` against `cargo test --workspace` output.
//!
//! Expected: graph-native discovery finds >= 90% of tests that cargo discovers.

use nusy_codegraph::ingest_pipeline::ingest_workspace;
use nusy_codegraph::test_discovery::{discover_tests, discovery_summary};
use nusy_cranelift::test_runner::run_tests_for_crate;
use nusy_cranelift::{CachedWasmCompiler, wasm_compiler::WasmCompiler};
use std::path::Path;

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

/// Get cargo test count by parsing `cargo test --workspace -- --list` output.
fn cargo_test_count() -> usize {
    let output = std::process::Command::new("cargo")
        .args(["test", "--workspace", "--", "--list"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo test --list");

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().filter(|l| l.ends_with(": test")).count()
}

#[test]
fn phase4_discovery_parity_check() {
    let graph = ingest_workspace(workspace_root());
    let nodes_batch = graph.merged_nodes_batch().expect("merged nodes");
    let tests = discover_tests(&[nodes_batch]);
    let summary = discovery_summary(&tests);

    let graph_total = summary.total_tests;
    eprintln!("\n=== Graph-Native Test Discovery ===");
    eprintln!("{summary}");

    let cargo_total = cargo_test_count();
    eprintln!("=== Cargo Test Discovery ===");
    eprintln!("cargo test --list: {cargo_total} tests\n");

    if cargo_total > 0 {
        let parity_pct = (graph_total as f64 / cargo_total as f64) * 100.0;
        eprintln!("Parity: {graph_total}/{cargo_total} = {parity_pct:.1}% (target: >= 90%)");

        eprintln!("\n=== Per-Crate Breakdown ===");
        for (crate_name, count) in &summary.crates {
            eprintln!("  {crate_name}: {count} tests (graph)");
        }

        assert!(
            parity_pct >= 90.0,
            "Graph discovery found {graph_total}/{cargo_total} tests ({parity_pct:.1}%) — below 90% target"
        );
    }
}

#[test]
fn phase4_sandbox_execution_sample() {
    let graph = ingest_workspace(workspace_root());
    let nodes_batch = graph.merged_nodes_batch().expect("merged nodes");
    let tests = discover_tests(&[nodes_batch]);

    if let Some(nodes) = tests.get("nusy-cranelift") {
        let compiler = WasmCompiler::new().expect("WasmCompiler");
        let mut cached = CachedWasmCompiler::new(compiler);

        let report = run_tests_for_crate(&mut cached, "nusy-cranelift", nodes);
        eprintln!("\n=== Sandbox Execution Report ===");
        eprintln!("{}", report.format());
        eprintln!(
            "\nTotal: {}, Passed: {}, Skipped: {}, Failed: {}",
            report.total,
            report.passed,
            report.skipped,
            report.failed.len()
        );
    } else {
        eprintln!("No tests found for nusy-cranelift — skipping sandbox execution sample");
    }
}
