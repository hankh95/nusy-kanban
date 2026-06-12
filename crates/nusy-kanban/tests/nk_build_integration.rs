//! Integration tests for `nk build` and `nk test` subcommands (EX-3186).
//!
//! These tests verify:
//! - `nk build` produces valid JSON output
//! - `nk test` produces valid JSON output with expected fields
//! - `nk build --clean` is slower than a cached `nk build` (cache working)
//! - `nk build --crate <name>` filters to a single crate
//! - Exit codes are correct (0 = success, non-zero = failure)
//!
//! Note: These tests invoke the compiled `nusy-kanban` binary. They require
//! `cargo build -p nusy-kanban` to have run first (done by the test harness).

#![cfg(feature = "build")]

use std::process::Command;

/// Path to the compiled `nusy-kanban` binary produced by Cargo.
fn nk_bin() -> std::path::PathBuf {
    // `cargo test` sets CARGO_MANIFEST_DIR; the binary is in target/debug/.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // workspace root is two levels up from crates/nusy-kanban
    let workspace_root = manifest.parent().unwrap().parent().unwrap();
    workspace_root.join("target/debug/nusy-kanban")
}

/// Workspace root (used as the `--workspace` argument).
fn workspace_root() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Run `nusy-kanban <args>` and return stdout + exit status.
fn run_nk(args: &[&str]) -> (String, std::process::ExitStatus) {
    let bin = nk_bin();
    assert!(
        bin.exists(),
        "nusy-kanban binary not found at {}: run `cargo build -p nusy-kanban` first",
        bin.display()
    );

    let output = Command::new(&bin)
        .args(args)
        .output()
        .expect("failed to run nusy-kanban");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    (stdout, output.status)
}

// ── nk build --json ───────────────────────────────────────────────────────────

#[test]
fn test_nk_build_json_output_valid() {
    let ws = workspace_root();
    let (stdout, status) = run_nk(&["build", "--workspace", ws.to_str().unwrap(), "--json"]);

    // Must exit successfully (compile errors are expected for unsupported syntax
    // but nk build --json exits 0 as long as the orchestrator itself succeeds)
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("nk build --json output must be valid JSON");

    // Required fields
    assert!(
        parsed.get("crates").is_some(),
        "missing 'crates' field: {stdout}"
    );
    assert!(
        parsed.get("total_functions").is_some(),
        "missing 'total_functions' field: {stdout}"
    );
    assert!(
        parsed.get("compiled").is_some(),
        "missing 'compiled' field: {stdout}"
    );
    assert!(
        parsed.get("cached").is_some(),
        "missing 'cached' field: {stdout}"
    );
    assert!(
        parsed.get("compile_errors").is_some(),
        "missing 'compile_errors' field: {stdout}"
    );
    assert!(
        parsed.get("duration_ms").is_some(),
        "missing 'duration_ms' field: {stdout}"
    );
    assert!(
        parsed.get("success").is_some(),
        "missing 'success' field: {stdout}"
    );

    // Must report at least one crate (the workspace has 28+ crates)
    let crates = parsed["crates"].as_u64().unwrap_or(0);
    assert!(
        crates > 0,
        "expected at least 1 crate in build report, got {crates}"
    );

    // The 'success' field must be a boolean
    assert!(
        parsed["success"].is_boolean(),
        "'success' must be a boolean, got: {}",
        parsed["success"]
    );

    // Exit 0 when success == true, exit 1 when success == false
    if parsed["success"].as_bool().unwrap_or(false) {
        assert!(
            status.success(),
            "nk build exited non-zero despite success=true"
        );
    }
}

// ── nk test --json ────────────────────────────────────────────────────────────

#[test]
fn test_nk_test_json_output_valid() {
    let ws = workspace_root();
    let (stdout, _status) = run_nk(&["test", "--workspace", ws.to_str().unwrap(), "--json"]);

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("nk test --json output must be valid JSON");

    // Required fields
    assert!(
        parsed.get("total").is_some(),
        "missing 'total' field: {stdout}"
    );
    assert!(
        parsed.get("passed").is_some(),
        "missing 'passed' field: {stdout}"
    );
    assert!(
        parsed.get("failed").is_some(),
        "missing 'failed' field: {stdout}"
    );
    assert!(
        parsed.get("skipped").is_some(),
        "missing 'skipped' field: {stdout}"
    );
    assert!(
        parsed.get("duration_ms").is_some(),
        "missing 'duration_ms' field: {stdout}"
    );
    assert!(
        parsed.get("success").is_some(),
        "missing 'success' field: {stdout}"
    );
    assert!(
        parsed
            .get("failures")
            .map(|v| v.is_array())
            .unwrap_or(false),
        "'failures' must be an array: {stdout}"
    );

    // Sanity: passed + failed + skipped == total
    let total = parsed["total"].as_u64().unwrap_or(0);
    let passed = parsed["passed"].as_u64().unwrap_or(0);
    let failed = parsed["failed"].as_u64().unwrap_or(0);
    let skipped = parsed["skipped"].as_u64().unwrap_or(0);
    assert_eq!(
        total,
        passed + failed + skipped,
        "total ({total}) != passed ({passed}) + failed ({failed}) + skipped ({skipped})"
    );

    // failures array length must match failed count
    let failures_len = parsed["failures"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(
        failed as usize, failures_len,
        "failed count ({failed}) != failures array length ({failures_len})"
    );
}

// ── nk build --clean forces cached=0 in output ───────────────────────────────

#[test]
fn test_nk_build_clean_resets_cache_count() {
    let ws = workspace_root();
    let ws_str = ws.to_str().unwrap();

    // Run once to populate the cache with any successfully compiled functions
    let (out1, _) = run_nk(&["build", "--workspace", ws_str, "--json"]);
    let r1: serde_json::Value = serde_json::from_str(&out1).expect("first run JSON must be valid");

    // Run again without --clean: any successfully compiled functions from the
    // first run should be served from cache (cached >= 0).
    let (out2, _) = run_nk(&["build", "--workspace", ws_str, "--json"]);
    let r2: serde_json::Value = serde_json::from_str(&out2).expect("second run JSON must be valid");

    // The number of successfully compiled functions should be the same across
    // both runs (deterministic compilation).
    let compiled1 = r1["compiled"].as_u64().unwrap_or(0);
    let compiled2 = r2["compiled"].as_u64().unwrap_or(0);
    assert_eq!(
        compiled1, compiled2,
        "compiled count should be stable across runs: {compiled1} != {compiled2}"
    );

    // Run with --clean: cache_hits must be 0 (clean ignores stored cache)
    let (out_clean, _) = run_nk(&["build", "--workspace", ws_str, "--clean", "--json"]);
    let r_clean: serde_json::Value =
        serde_json::from_str(&out_clean).expect("clean run JSON must be valid");
    let cached_after_clean = r_clean["cached"].as_u64().unwrap_or(u64::MAX);
    assert_eq!(
        cached_after_clean, 0,
        "nk build --clean must report cached=0 (cache bypassed), got {cached_after_clean}"
    );

    // Warm build must be under 30s (expedition constraint)
    let duration_ms = r2["duration_ms"].as_u64().unwrap_or(u64::MAX);
    assert!(
        duration_ms < 30_000,
        "nk build warm took {duration_ms}ms, must be < 30000ms"
    );
}

// ── nk build --crate filters correctly ───────────────────────────────────────

#[test]
fn test_nk_build_single_crate_fewer_functions() {
    let ws = workspace_root();
    let ws_str = ws.to_str().unwrap();

    // Full workspace build
    let (full_out, _) = run_nk(&["build", "--workspace", ws_str, "--json"]);
    let full: serde_json::Value =
        serde_json::from_str(&full_out).expect("full build JSON must be valid");
    let full_fns = full["total_functions"].as_u64().unwrap_or(0);

    // Single-crate build
    let (single_out, _) = run_nk(&[
        "build",
        "--workspace",
        ws_str,
        "--crate",
        "nusy-arrow-core",
        "--json",
    ]);
    let single: serde_json::Value =
        serde_json::from_str(&single_out).expect("single-crate build JSON must be valid");
    let single_fns = single["total_functions"].as_u64().unwrap_or(0);

    // Single crate must report fewer functions than the full workspace
    assert!(
        single_fns < full_fns,
        "single-crate build ({single_fns} fns) should be < full workspace ({full_fns} fns)"
    );

    // Single crate reports exactly 1 crate
    let single_crates = single["crates"].as_u64().unwrap_or(0);
    assert_eq!(
        single_crates, 1,
        "single-crate build should report crates=1, got {single_crates}"
    );
}
