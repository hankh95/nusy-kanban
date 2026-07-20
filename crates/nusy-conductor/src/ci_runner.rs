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
    /// V19 per-PR correctness regression gate (CH-5069 / EX-4933 / VY-4930 E3):
    /// the zero-tolerance invariants (false_proofs=0, never-launder,
    /// proof-completeness, hallucination=0, loud-abstention) via
    /// `scripts/regression-hard-gate.sh`. This makes the gate the fleet's
    /// pre-push hook already enforces *also* visible/blocking on the
    /// authoritative `nk pr checks` surface (GitHub Actions do NOT fire for
    /// graph-native `nk pr` proposals).
    Regression,
    /// V19 eval-data provenance guard (EX-5195 / VY-5191 E4): asserts that any
    /// eval-data JSON change carries a well-formed E2 provenance block
    /// (expr_id, git_sha, run_command, ran_at, ran_by) with a fresh git_sha.
    /// Mirrors the pre-push hook's provenance gate on the authoritative
    /// `nk pr checks` surface.
    Provenance,
}

impl std::fmt::Display for CheckType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckType::Test => write!(f, "test"),
            CheckType::Clippy => write!(f, "clippy"),
            CheckType::Fmt => write!(f, "fmt"),
            CheckType::Regression => write!(f, "regression"),
            CheckType::Provenance => write!(f, "provenance"),
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
    checks.push(run_regression_gate(repo_root));
    checks.push(run_provenance_gate(repo_root));

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

/// Run the V19 per-PR correctness regression gate (`scripts/regression-hard-gate.sh`).
///
/// The gate script is the single source of truth (CH-5069 / EX-4933 / VY-4930 E3):
/// it runs the regression runner's anti-tautology `--self-test` plus the per-PR
/// tier of zero-tolerance correctness invariants against the committed eval data,
/// and exits non-zero on ANY regression. Fast (<1s), no GPU. When its tooling is
/// absent (no python3/PyYAML/runner) the script exits 0 ("SKIPPED") so it never
/// strands a run — the pre-push hook (EX-4933) and release gate remain backstops.
fn run_regression_gate(repo_root: &Path) -> CheckResult {
    let start = Instant::now();
    let gate = repo_root.join("scripts/regression-hard-gate.sh");

    // Gate script not present in this checkout → clean skip (do not block).
    if !gate.is_file() {
        return CheckResult {
            check_type: CheckType::Regression,
            passed: true,
            summary: "skipped (gate script absent)".to_string(),
            output: String::new(),
            duration: start.elapsed(),
        };
    }

    let output = Command::new("bash")
        .arg(&gate)
        .current_dir(repo_root)
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            let passed = output.status.success();

            let summary = if combined.contains("SKIPPED") {
                // The script self-skips (exit 0) when python3/PyYAML/the runner are
                // absent; surface that rather than implying the invariants were checked.
                "skipped (tooling absent; pre-push + release gate enforce)".to_string()
            } else {
                parse_regression_summary(&combined).unwrap_or_else(|| {
                    if passed {
                        "correctness invariants hold".to_string()
                    } else {
                        "regression detected".to_string()
                    }
                })
            };

            CheckResult {
                check_type: CheckType::Regression,
                passed,
                summary,
                output: truncate_output(&combined, 4000),
                duration: start.elapsed(),
            }
        }
        Err(e) => CheckResult {
            check_type: CheckType::Regression,
            passed: false,
            summary: format!("failed to run: {e}"),
            output: String::new(),
            duration: start.elapsed(),
        },
    }
}

/// Run the provenance guard script (scripts/provenance-guard.py) against
/// eval-data JSON changes relative to origin/main.  Mirrors
/// `run_regression_gate` exactly: absent script = clean skip; python3 not
/// found = clean skip; FAIL exit = violation surfaced in `nk pr checks`.
fn run_provenance_gate(repo_root: &Path) -> CheckResult {
    let start = Instant::now();
    let guard = repo_root.join("scripts/provenance-guard.py");

    if !guard.is_file() {
        return CheckResult {
            check_type: CheckType::Provenance,
            passed: true,
            summary: "skipped (guard script absent)".to_string(),
            output: String::new(),
            duration: start.elapsed(),
        };
    }

    let output = Command::new("python3")
        .arg(&guard)
        .current_dir(repo_root)
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            let passed = output.status.success();

            let summary = if combined.contains("SKIP_PROVENANCE_GATE") {
                "skipped (SKIP_PROVENANCE_GATE set)".to_string()
            } else if combined.contains("no eval-data JSON changes") {
                "no eval-data changes".to_string()
            } else {
                parse_provenance_summary(&combined).unwrap_or_else(|| {
                    if passed {
                        "provenance blocks valid".to_string()
                    } else {
                        "provenance violation detected".to_string()
                    }
                })
            };

            CheckResult {
                check_type: CheckType::Provenance,
                passed,
                summary,
                output: truncate_output(&combined, 4000),
                duration: start.elapsed(),
            }
        }
        Err(e) => {
            // python3 not available → skip gracefully (same policy as the gate's
            // own python3-absent self-skip: never block on tooling absence).
            if e.kind() == std::io::ErrorKind::NotFound {
                return CheckResult {
                    check_type: CheckType::Provenance,
                    passed: true,
                    summary: "skipped (python3 absent)".to_string(),
                    output: String::new(),
                    duration: start.elapsed(),
                };
            }
            CheckResult {
                check_type: CheckType::Provenance,
                passed: false,
                summary: format!("failed to run: {e}"),
                output: String::new(),
                duration: start.elapsed(),
            }
        }
    }
}

/// Parse the provenance guard's VERDICT line into a short summary, e.g.
/// "[provenance-guard] VERDICT: PASS 3 FAIL 0" → "3 file(s) checked, 0 violated".
/// Returns `None` if no verdict line is found (caller falls back to generic summary).
fn parse_provenance_summary(out: &str) -> Option<String> {
    for line in out.lines().rev() {
        let l = line.trim();
        if l.contains("[provenance-guard] VERDICT:") {
            let toks: Vec<&str> = l.split_whitespace().collect();
            let after = |kw: &str| -> Option<u32> {
                toks.iter()
                    .enumerate()
                    .filter(|(_, t)| **t == kw)
                    .find_map(|(i, _)| toks.get(i + 1).and_then(|n| n.parse::<u32>().ok()))
            };
            if let (Some(p), Some(f)) = (after("PASS"), after("FAIL")) {
                return Some(format!("{p} file(s) checked, {f} violated"));
            }
        }
    }
    None
}

/// Parse the regression runner's final verdict line into a short summary, e.g.
/// "PASS — PASS 8 FAIL 0 ERROR 0 PENDING 0  (report: …)" → "8 invariant(s) held, 0 regressed".
/// Returns `None` if no verdict line is found (caller falls back to a generic summary).
fn parse_regression_summary(out: &str) -> Option<String> {
    for line in out.lines().rev() {
        let l = line.trim();
        if (l.starts_with("PASS ") || l.starts_with("FAIL ")) && l.contains("FAIL ") {
            let toks: Vec<&str> = l.split_whitespace().collect();
            // Find the first occurrence of the keyword whose NEXT token parses as a
            // count (the leading token is the overall verdict, e.g. "PASS — PASS 8 …").
            let after = |kw: &str| -> Option<u32> {
                toks.iter()
                    .enumerate()
                    .filter(|(_, t)| **t == kw)
                    .find_map(|(i, _)| toks.get(i + 1).and_then(|n| n.parse::<u32>().ok()))
            };
            if let (Some(p), Some(f)) = (after("PASS"), after("FAIL")) {
                return Some(format!("{p} invariant(s) held, {f} regressed"));
            }
        }
    }
    None
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
        assert_eq!(CheckType::Regression.to_string(), "regression");
    }

    #[test]
    fn test_parse_regression_summary_clean() {
        // The runner's final line on a clean per-pr run.
        let out = "  PASS    REG-002  H-4737   base=0  cur=0  all == 0\n\
                   PASS — PASS 8 FAIL 0 ERROR 0 PENDING 0  (report: research/.../regression-report.json)";
        assert_eq!(
            parse_regression_summary(out),
            Some("8 invariant(s) held, 0 regressed".to_string())
        );
    }

    #[test]
    fn test_parse_regression_summary_seeded_fail() {
        let out = "  FAIL    REG-002  H-4737   base=0  cur=3  [3.0] != baseline 0\n\
                   FAIL — PASS 7 FAIL 1 ERROR 0 PENDING 0  (report: …)";
        assert_eq!(
            parse_regression_summary(out),
            Some("7 invariant(s) held, 1 regressed".to_string())
        );
    }

    #[test]
    fn test_parse_regression_summary_none() {
        assert_eq!(parse_regression_summary("no verdict line here"), None);
    }

    #[test]
    fn test_run_regression_gate_absent_script_is_clean_skip() {
        // A checkout without the gate script must NOT block (clean skip).
        let dir = std::env::temp_dir();
        let result = run_regression_gate(&dir);
        assert_eq!(result.check_type, CheckType::Regression);
        assert!(result.passed, "absent gate script must not block");
        assert!(result.summary.contains("skipped"));
    }

    #[test]
    fn test_ci_suite_summary_includes_regression_line() {
        let suite = CiCheckSuite {
            checks: vec![CheckResult {
                check_type: CheckType::Regression,
                passed: false,
                summary: "3 invariant(s) held, 1 regressed".to_string(),
                output: String::new(),
                duration: Duration::from_millis(500),
            }],
            passed: false,
            total_duration: Duration::from_millis(500),
            error: None,
        };
        let summary = suite.summary();
        assert!(summary.contains("regression:"));
        assert!(summary.contains("1 regressed"));
        assert!(summary.contains("FAILED"));
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
