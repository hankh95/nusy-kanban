//! EX-3181: Integration tests for the graph-native build orchestrator.
//!
//! These tests ingest the real NuSy workspace and exercise the build
//! orchestrator end-to-end.

use std::path::Path;

use nusy_codegraph::ingest_pipeline::ingest_workspace;
use nusy_cranelift::build_orchestrator::{BuildConfig, BuildOrchestrator};

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

// ── Test 1: full workspace build from graph ──────────────────────────────────

#[test]
fn test_full_workspace_build() {
    let root = workspace_root();
    let ingest_result = ingest_workspace(root);

    assert!(
        ingest_result.total_nodes() >= 1_000,
        "expected >= 1000 CodeNodes from workspace, got {}",
        ingest_result.total_nodes()
    );

    let config = BuildConfig {
        run_tests: false,
        ..BuildConfig::default()
    };

    let mut orchestrator = BuildOrchestrator::new().expect("orchestrator init");
    let report = orchestrator
        .build_from_graph(root, &ingest_result, &config)
        .expect("build_from_graph should succeed");

    // Verify report structure
    assert!(
        !report.crate_reports.is_empty(),
        "should have at least one crate report"
    );
    assert!(
        report.total_functions > 0,
        "should have compiled some functions"
    );
    assert!(
        report.compiled > 0,
        "should have compiled at least one function"
    );
    assert!(report.total_duration_ms > 0, "build should take some time");

    // The format() method should produce readable output
    let formatted = report.format();
    assert!(formatted.contains("graph.build()"), "report header");
    assert!(formatted.contains("Crates built:"), "crate count");
    assert!(formatted.contains("Total functions:"), "function count");

    eprintln!("\n{formatted}");
}

// ── Test 2: build + run tests ────────────────────────────────────────────────

#[test]
fn test_build_with_tests() {
    let root = workspace_root();
    let ingest_result = ingest_workspace(root);

    let config = BuildConfig {
        run_tests: true,
        // Filter to a small crate to keep test time reasonable
        crate_filter: Some("nusy-arrow-core".to_string()),
        ..BuildConfig::default()
    };

    let mut orchestrator = BuildOrchestrator::new().expect("orchestrator init");
    let report = orchestrator
        .build_from_graph(root, &ingest_result, &config)
        .expect("build_from_graph should succeed");

    // Should have exactly one crate report (filtered)
    assert_eq!(
        report.crate_reports.len(),
        1,
        "filtered build should have exactly one crate report"
    );
    assert_eq!(
        report.crate_reports[0].crate_name, "nusy-arrow-core",
        "should be the filtered crate"
    );

    // Test reports may or may not be present depending on whether tests
    // are compilable in the WASM sandbox (many won't be — that's expected)
    let formatted = report.format();
    eprintln!("\n{formatted}");

    // The report should mention the crate
    assert!(
        formatted.contains("nusy-arrow-core"),
        "report should mention filtered crate"
    );
}

// ── Test 3: incremental build shows cache improvement ────────────────────────

#[test]
fn test_incremental_build_from_graph() {
    let root = workspace_root();
    let ingest_result = ingest_workspace(root);

    let config = BuildConfig {
        run_tests: false,
        crate_filter: Some("nusy-arrow-core".to_string()),
        ..BuildConfig::default()
    };

    let mut orchestrator = BuildOrchestrator::new().expect("orchestrator init");

    // First build: all misses (some succeed, some error)
    let r1 = orchestrator
        .build_from_graph(root, &ingest_result, &config)
        .expect("first build");

    let first_misses: u64 = r1.crate_reports.iter().map(|r| r.build.cache_misses).sum();
    let first_errors: usize = r1.crate_reports.iter().map(|r| r.build.errors.len()).sum();
    let first_successful = first_misses as usize - first_errors;

    assert!(first_misses > 0, "first build should have cache misses");

    // Second build with same data:
    // - Successfully compiled functions are served from cache (hits)
    // - Functions that errored will error again (counted as misses, not cached)
    let r2 = orchestrator
        .build_from_graph(root, &ingest_result, &config)
        .expect("second build");

    let second_misses: u64 = r2.crate_reports.iter().map(|r| r.build.cache_misses).sum();
    let second_hits: u64 = r2.crate_reports.iter().map(|r| r.build.cache_hits).sum();

    // Cache hits on second build should match successful compiles from first build
    assert_eq!(
        second_hits as usize, first_successful,
        "second build hits ({second_hits}) should equal first build successes ({first_successful})"
    );

    // If there were any successful compiles, the second build should be faster
    if first_successful > 0 {
        assert!(second_hits > 0, "should have cache hits on second build");
    }

    eprintln!(
        "Incremental: 1st build {first_misses} misses ({first_errors} errors, {first_successful} ok), \
         2nd build {second_misses} misses / {second_hits} hits"
    );
}
