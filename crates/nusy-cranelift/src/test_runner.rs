//! Graph-native test runner — compile and execute `#[test]` functions in WASM sandbox.
//!
//! EX-3180 Phase 2: Uses `CachedWasmCompiler` to compile test CodeNodes and
//! execute them. Tests that panic or timeout produce `TestResult::Failed` or
//! `TestResult::Timeout`.
//!
//! ## Limitations
//!
//! The WASM sandbox supports a restricted Rust DSL subset (primitives, arithmetic,
//! comparisons, let, if/else). Tests requiring external I/O (NATS, filesystem,
//! network), async, or complex types will produce `CompileError`. This is expected
//! and documented — not all tests can run in the sandbox.

use std::time::Instant;

use nusy_codegraph::schema::CodeNode;

use crate::cached_compiler::CachedWasmCompiler;
use crate::error::CraneliftError;
use crate::wasm_compiler::WasmValue;

/// Result of executing a single test.
#[derive(Debug, Clone)]
pub enum TestResult {
    /// Test executed without error (returned normally).
    Passed,
    /// Test panicked or assertion failed.
    Failed { message: String },
    /// Test exceeded the timeout.
    Timeout,
    /// Test could not be compiled (unsupported syntax, parse error).
    CompileError { message: String },
}

impl TestResult {
    pub fn is_passed(&self) -> bool {
        matches!(self, TestResult::Passed)
    }

    pub fn status_str(&self) -> &'static str {
        match self {
            TestResult::Passed => "ok",
            TestResult::Failed { .. } => "FAILED",
            TestResult::Timeout => "TIMEOUT",
            TestResult::CompileError { .. } => "COMPILE_ERROR",
        }
    }
}

/// Report for a crate's test suite.
#[derive(Debug)]
pub struct TestSuiteReport {
    pub crate_name: String,
    pub total: usize,
    pub passed: usize,
    pub failed: Vec<(String, TestResult)>,
    pub skipped: usize,
    pub duration_ms: u64,
}

impl TestSuiteReport {
    /// Format as cargo-test-style output.
    pub fn format(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "running {} tests for {}",
            self.total, self.crate_name
        ));

        // Show passed tests as "ok"
        // Show failed tests with details
        for (name, result) in &self.failed {
            lines.push(format!("test {} ... {}", name, result.status_str()));
            if let TestResult::Failed { message } = result {
                lines.push(format!("  {message}"));
            }
            if let TestResult::CompileError { message } = result {
                lines.push(format!("  {message}"));
            }
        }

        let status = if self.failed.is_empty() {
            "ok"
        } else {
            "FAILED"
        };
        lines.push(format!(
            "\ntest result: {status}. {} passed; {} failed; {} skipped; finished in {:.1}s",
            self.passed,
            self.failed.len(),
            self.skipped,
            self.duration_ms as f64 / 1000.0,
        ));

        lines.join("\n")
    }
}

/// Run a single test CodeNode in the WASM sandbox.
///
/// Test functions are expected to be `pub fn test_name() -> i64 { ... ; 0 }`.
/// A return value of 0 means passed. Any panic or non-zero return is a failure.
///
/// Tests that can't compile (unsupported syntax) return `CompileError` — these
/// are expected for tests using async, complex types, or external I/O.
pub fn run_single_test(compiler: &mut CachedWasmCompiler, test_node: &CodeNode) -> TestResult {
    match compiler.compile_and_run(test_node, &[]) {
        Ok(WasmValue::I64(0)) => TestResult::Passed,
        Ok(val) => TestResult::Failed {
            message: format!("test returned non-zero: {val:?}"),
        },
        Err(CraneliftError::ExecutionTimeout(_)) => TestResult::Timeout,
        Err(CraneliftError::UnsupportedSyntax(msg)) => TestResult::CompileError {
            message: format!("unsupported: {msg}"),
        },
        Err(CraneliftError::ParseError(msg)) => TestResult::CompileError {
            message: format!("parse: {msg}"),
        },
        Err(e) => TestResult::Failed {
            message: e.to_string(),
        },
    }
}

/// Run all tests for a crate.
pub fn run_tests_for_crate(
    compiler: &mut CachedWasmCompiler,
    crate_name: &str,
    test_nodes: &[CodeNode],
) -> TestSuiteReport {
    let start = Instant::now();
    let mut passed = 0usize;
    let mut skipped = 0usize;
    let mut failed = Vec::new();

    for node in test_nodes {
        // Skip tests without bodies
        if node.body.is_none() {
            skipped += 1;
            continue;
        }

        let result = run_single_test(compiler, node);
        match &result {
            TestResult::Passed => passed += 1,
            TestResult::CompileError { .. } => {
                // CompileError = unsupported syntax, count as skipped not failed
                skipped += 1;
            }
            _ => {
                failed.push((node.name.clone(), result));
            }
        }
    }

    TestSuiteReport {
        crate_name: crate_name.to_string(),
        total: test_nodes.len(),
        passed,
        failed,
        skipped,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

/// Run all tests across all crates.
pub fn run_all_tests(
    compiler: &mut CachedWasmCompiler,
    tests_by_crate: &std::collections::HashMap<String, Vec<CodeNode>>,
) -> Vec<TestSuiteReport> {
    let mut reports = Vec::new();
    let mut crate_names: Vec<&String> = tests_by_crate.keys().collect();
    crate_names.sort();

    for crate_name in crate_names {
        let nodes = &tests_by_crate[crate_name];
        reports.push(run_tests_for_crate(compiler, crate_name, nodes));
    }
    reports
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusy_codegraph::schema::CodeNodeKind;

    fn make_test(name: &str, body: &str) -> CodeNode {
        CodeNode {
            id: name.into(),
            kind: CodeNodeKind::RustTest,
            name: name.into(),
            body: Some(body.into()),
            ..CodeNode::default()
        }
    }

    fn compiler() -> CachedWasmCompiler {
        CachedWasmCompiler::new(crate::wasm_compiler::WasmCompiler::new().expect("WasmCompiler"))
    }

    #[test]
    fn test_passing_test() {
        let mut c = compiler();
        let node = make_test("test_ok", "pub fn test_ok() -> i64 { 0 }");
        let result = run_single_test(&mut c, &node);
        assert!(result.is_passed());
    }

    #[test]
    fn test_failing_test_nonzero_return() {
        let mut c = compiler();
        let node = make_test("test_fail", "pub fn test_fail() -> i64 { 1 }");
        let result = run_single_test(&mut c, &node);
        assert!(matches!(result, TestResult::Failed { .. }));
    }

    #[test]
    fn test_compile_error_for_unsupported_syntax() {
        let mut c = compiler();
        let node = make_test(
            "test_async",
            "pub async fn test_async() { tokio::time::sleep(Duration::from_secs(1)).await; }",
        );
        let result = run_single_test(&mut c, &node);
        assert!(
            matches!(result, TestResult::CompileError { .. }),
            "async test should produce CompileError"
        );
    }

    #[test]
    fn test_suite_report() {
        let mut c = compiler();
        let nodes = vec![
            make_test("test_ok1", "pub fn test_ok1() -> i64 { 0 }"),
            make_test("test_ok2", "pub fn test_ok2() -> i64 { 0 }"),
            make_test("test_fail", "pub fn test_fail() -> i64 { 42 }"),
        ];

        let report = run_tests_for_crate(&mut c, "test-crate", &nodes);
        assert_eq!(report.total, 3);
        assert_eq!(report.passed, 2);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "test_fail");
    }

    #[test]
    fn test_suite_skips_no_body() {
        let mut c = compiler();
        let nodes = vec![
            make_test("test_ok", "pub fn test_ok() -> i64 { 0 }"),
            CodeNode {
                id: "stub".into(),
                kind: CodeNodeKind::RustTest,
                name: "stub".into(),
                body: None,
                ..CodeNode::default()
            },
        ];

        let report = run_tests_for_crate(&mut c, "test-crate", &nodes);
        assert_eq!(report.total, 2);
        assert_eq!(report.passed, 1);
        assert_eq!(report.skipped, 1);
    }

    #[test]
    fn test_compile_error_counts_as_skipped() {
        let mut c = compiler();
        let nodes = vec![
            make_test("test_ok", "pub fn test_ok() -> i64 { 0 }"),
            make_test(
                "test_complex",
                "pub fn test_complex() { let v: Vec<i32> = vec![]; }",
            ),
        ];

        let report = run_tests_for_crate(&mut c, "test-crate", &nodes);
        assert_eq!(report.passed, 1);
        assert_eq!(report.skipped, 1); // complex test skipped, not failed
        assert!(report.failed.is_empty());
    }

    #[test]
    fn test_format_report() {
        let report = TestSuiteReport {
            crate_name: "my-crate".to_string(),
            total: 10,
            passed: 8,
            failed: vec![(
                "test_bad".to_string(),
                TestResult::Failed {
                    message: "wrong".into(),
                },
            )],
            skipped: 1,
            duration_ms: 1234,
        };

        let output = report.format();
        assert!(output.contains("running 10 tests"));
        assert!(output.contains("my-crate"));
        assert!(output.contains("8 passed"));
        assert!(output.contains("1 failed"));
        assert!(output.contains("1 skipped"));
    }
}
