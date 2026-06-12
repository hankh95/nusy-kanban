//! CI runner — executes cargo test, clippy, and fmt checks.
//!
//! Provides synchronous execution of Rust workspace checks,
//! returning structured results that can be stored in Arrow tables
//! and displayed by `nk pr checks`.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Result of a single CI check (test, clippy, or fmt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// Which check was run.
    pub check_type: CheckType,
    /// Whether the check passed.
    pub passed: bool,
    /// Human-readable summary (e.g., "74 tests passed").
    pub summary: String,
    /// Full output (truncated for storage).
    pub output: String,
    /// How long the check took.
    pub duration: Duration,
}

/// Types of CI checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckType {
    Test,
    Clippy,
    Fmt,
}

impl std::fmt::Display for CheckType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckType::Test => write!(f, "test"),
            CheckType::Clippy => write!(f, "clippy"),
            CheckType::Fmt => write!(f, "fmt"),
        }
    }
}

/// Aggregate result of all CI checks for a proposal.
#[derive(Debug, Clone)]
pub struct CiCheckSuite {
    /// Individual check results.
    pub checks: Vec<CheckResult>,
    /// Overall pass/fail.
    pub passed: bool,
    /// Total duration across all checks.
    pub total_duration: Duration,
    /// Error message if the suite couldn't run at all.
    pub error: Option<String>,
}

impl CiCheckSuite {
    /// Format as a human-readable summary.
    pub fn summary(&self) -> String {
        if let Some(ref err) = self.error {
            return format!("CI error: {err}");
        }
        let status = if self.passed { "PASSED" } else { "FAILED" };
        let details: Vec<String> = self
            .checks
            .iter()
            .map(|c| {
                let icon = if c.passed { "✓" } else { "✗" };
                format!("{icon} {}: {}", c.check_type, c.summary)
            })
            .collect();
        format!(
            "CI {status} ({:.1}s)\n{}",
            self.total_duration.as_secs_f64(),
            details.join("\n")
        )
    }
}

/// Run all CI checks on a workspace directory.
///
/// Executes `cargo test`, `cargo clippy`, and `cargo fmt --check`
/// in the given directory. Returns structured results.
pub fn run_ci_checks(repo_root: &Path) -> CiCheckSuite {
    if !repo_root.is_dir() {
        return CiCheckSuite {
            checks: vec![],
            passed: false,
            total_duration: Duration::ZERO,
            error: Some(format!("directory not found: {}", repo_root.display())),
        };
    }

    let mut checks = Vec::new();
    let suite_start = Instant::now();

    checks.push(run_cargo_test(repo_root));
    checks.push(run_cargo_clippy(repo_root));
    checks.push(run_cargo_fmt(repo_root));

    let passed = checks.iter().all(|c| c.passed);
    let total_duration = suite_start.elapsed();

    CiCheckSuite {
        checks,
        passed,
        total_duration,
        error: None,
    }
}

/// Run `cargo test --workspace` and parse the output.
fn run_cargo_test(repo_root: &Path) -> CheckResult {
    let start = Instant::now();
    let output = Command::new("cargo")
        .args(["test", "--workspace"])
        .current_dir(repo_root)
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            let passed = output.status.success();

            let summary = parse_test_summary(&combined).unwrap_or_else(|| {
                if passed {
                    "all tests passed".to_string()
                } else {
                    "tests failed".to_string()
                }
            });

            CheckResult {
                check_type: CheckType::Test,
                passed,
                summary,
                output: truncate_output(&combined, 4000),
                duration: start.elapsed(),
            }
        }
        Err(e) => CheckResult {
            check_type: CheckType::Test,
            passed: false,
            summary: format!("failed to run: {e}"),
            output: String::new(),
            duration: start.elapsed(),
        },
    }
}

/// Run `cargo clippy --workspace -- -D warnings` and parse the output.
fn run_cargo_clippy(repo_root: &Path) -> CheckResult {
    let start = Instant::now();
    let output = Command::new("cargo")
        .args(["clippy", "--workspace", "--", "-D", "warnings"])
        .current_dir(repo_root)
        .output();

    match output {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let passed = output.status.success();

            let warning_count = stderr.matches("warning:").count();
            let summary = if passed {
                "no warnings".to_string()
            } else {
                format!("{warning_count} warning(s)")
            };

            CheckResult {
                check_type: CheckType::Clippy,
                passed,
                summary,
                output: truncate_output(&stderr, 4000),
                duration: start.elapsed(),
            }
        }
        Err(e) => CheckResult {
            check_type: CheckType::Clippy,
            passed: false,
            summary: format!("failed to run: {e}"),
            output: String::new(),
            duration: start.elapsed(),
        },
    }
}

/// Run `cargo fmt --all --check` and parse the output.
fn run_cargo_fmt(repo_root: &Path) -> CheckResult {
    let start = Instant::now();
    let output = Command::new("cargo")
        .args(["fmt", "--all", "--check"])
        .current_dir(repo_root)
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            let passed = output.status.success();

            let summary = if passed {
                "clean".to_string()
            } else {
                let diff_count = combined.matches("Diff in").count();
                if diff_count > 0 {
                    format!("{diff_count} file(s) need formatting")
                } else {
                    "formatting issues found".to_string()
                }
            };

            CheckResult {
                check_type: CheckType::Fmt,
                passed,
                summary,
                output: truncate_output(&combined, 2000),
                duration: start.elapsed(),
            }
        }
        Err(e) => CheckResult {
            check_type: CheckType::Fmt,
            passed: false,
            summary: format!("failed to run: {e}"),
            output: String::new(),
            duration: start.elapsed(),
        },
    }
}

/// Parse the "test result:" lines from cargo test output.
fn parse_test_summary(output: &str) -> Option<String> {
    let mut total_passed = 0u32;
    let mut total_failed = 0u32;
    let mut total_ignored = 0u32;

    for line in output.lines() {
        if line.starts_with("test result:") {
            // Format: "test result: ok. 42 passed; 0 failed; 1 ignored; ..."
            if let Some(passed) = extract_count(line, "passed") {
                total_passed += passed;
            }
            if let Some(failed) = extract_count(line, "failed") {
                total_failed += failed;
            }
            if let Some(ignored) = extract_count(line, "ignored") {
                total_ignored += ignored;
            }
        }
    }

    if total_passed == 0 && total_failed == 0 {
        return None;
    }

    let mut parts = vec![format!("{total_passed} passed")];
    if total_failed > 0 {
        parts.push(format!("{total_failed} failed"));
    }
    if total_ignored > 0 {
        parts.push(format!("{total_ignored} ignored"));
    }
    Some(parts.join(", "))
}

/// Extract a count from a "test result:" line (e.g., "42 passed").
fn extract_count(line: &str, label: &str) -> Option<u32> {
    let idx = line.find(label)?;
    let before = &line[..idx].trim_end();
    let num_str = before.rsplit([' ', ';']).next()?;
    num_str.trim().parse().ok()
}

/// Truncate output to a maximum number of bytes.
fn truncate_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let truncated = &s[..s.floor_char_boundary(max_bytes.saturating_sub(20))];
        format!("{truncated}\n... (truncated)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_test_summary_basic() {
        let output = "test result: ok. 42 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out";
        assert_eq!(
            parse_test_summary(output),
            Some("42 passed, 1 ignored".to_string())
        );
    }

    #[test]
    fn test_parse_test_summary_multiple_crates() {
        let output = "\
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out";
        assert_eq!(
            parse_test_summary(output),
            Some("18 passed, 2 failed".to_string())
        );
    }

    #[test]
    fn test_parse_test_summary_no_results() {
        assert_eq!(parse_test_summary("random output"), None);
    }

    #[test]
    fn test_truncate_output_short() {
        assert_eq!(truncate_output("hello", 100), "hello");
    }

    #[test]
    fn test_truncate_output_long() {
        let long = "a".repeat(5000);
        let result = truncate_output(&long, 100);
        assert!(result.len() <= 120);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_check_type_display() {
        assert_eq!(CheckType::Test.to_string(), "test");
        assert_eq!(CheckType::Clippy.to_string(), "clippy");
        assert_eq!(CheckType::Fmt.to_string(), "fmt");
    }

    #[test]
    fn test_ci_suite_summary_error() {
        let suite = CiCheckSuite {
            checks: vec![],
            passed: false,
            total_duration: Duration::ZERO,
            error: Some("dir not found".to_string()),
        };
        assert!(suite.summary().contains("CI error"));
    }

    #[test]
    fn test_ci_suite_summary_all_pass() {
        let suite = CiCheckSuite {
            checks: vec![
                CheckResult {
                    check_type: CheckType::Test,
                    passed: true,
                    summary: "42 passed".to_string(),
                    output: String::new(),
                    duration: Duration::from_secs(5),
                },
                CheckResult {
                    check_type: CheckType::Clippy,
                    passed: true,
                    summary: "no warnings".to_string(),
                    output: String::new(),
                    duration: Duration::from_secs(2),
                },
                CheckResult {
                    check_type: CheckType::Fmt,
                    passed: true,
                    summary: "clean".to_string(),
                    output: String::new(),
                    duration: Duration::from_secs(1),
                },
            ],
            passed: true,
            total_duration: Duration::from_secs(8),
            error: None,
        };
        let summary = suite.summary();
        assert!(summary.contains("PASSED"));
        assert!(summary.contains("42 passed"));
        assert!(summary.contains("no warnings"));
    }

    #[test]
    fn test_run_ci_checks_nonexistent_dir() {
        let suite = run_ci_checks(Path::new("/nonexistent/path"));
        assert!(!suite.passed);
        assert!(suite.error.is_some());
    }

    #[test]
    fn test_extract_count() {
        assert_eq!(extract_count("ok. 42 passed; 0 failed", "passed"), Some(42));
        assert_eq!(extract_count("ok. 42 passed; 3 failed", "failed"), Some(3));
        assert_eq!(extract_count("no match here", "passed"), None);
    }
}
