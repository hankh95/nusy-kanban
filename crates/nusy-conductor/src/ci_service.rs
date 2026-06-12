//! NATS CI service — runs cargo test/clippy/fmt on request, publishes results.
//!
//! Subscribes to `conductor.ci.request` for incoming CI requests (triggered by
//! `nk pr recheck` or proposal detection). Executes checks via [`ci_runner`],
//! then publishes structured results to `conductor.ci.result`.
//!
//! # NATS Subjects
//!
//! - **Request:** `conductor.ci.request` — JSON payload with `proposal_id` and `branch`
//! - **Result:** `conductor.ci.result` — JSON payload with full CI results
//!
//! # Architecture
//!
//! The service is designed to run as a long-lived background task on a build
//! agent (Mini or any machine with a Rust toolchain). It:
//!
//! 1. Subscribes to `conductor.ci.request`
//! 2. On each request: checks out the branch, runs all CI checks
//! 3. Publishes results to `conductor.ci.result`
//! 4. Replies to the original request with the result (request-reply pattern)

use futures::StreamExt;
use noesis_ship::connection::ConnectionManager;
use noesis_ship::event_bus::EventBus;
use noesis_ship::types::{NatsConfig, StreamConfig};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::ci_runner::CiCheckSuite;

// ── Request / Response types ─────────────────────────────────────────────────

/// A CI check request received over NATS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiRequest {
    /// The proposal ID (e.g., "PROP-2020").
    pub proposal_id: String,
    /// The git branch to check out and test.
    pub branch: String,
    /// Optional repository root override (defaults to current working dir).
    #[serde(default)]
    pub repo_root: Option<String>,
}

/// CI check result published over NATS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiResult {
    /// The proposal ID this result belongs to.
    pub proposal_id: String,
    /// The branch that was tested.
    pub branch: String,
    /// Overall pass/fail.
    pub passed: bool,
    /// Per-check results.
    pub checks: Vec<CiCheckResult>,
    /// Total duration in seconds.
    pub duration_secs: f64,
    /// Error message if the suite couldn't run at all.
    pub error: Option<String>,
    /// Human-readable summary.
    pub summary: String,
}

/// A single check result (test, clippy, or fmt) for NATS serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiCheckResult {
    /// Check type: "test", "clippy", or "fmt".
    pub check_type: String,
    /// Whether this check passed.
    pub passed: bool,
    /// Human-readable summary.
    pub summary: String,
    /// Duration in seconds.
    pub duration_secs: f64,
}

// ── NATS subjects ────────────────────────────────────────────────────────────

/// NATS subject for CI requests.
pub const CI_REQUEST_SUBJECT: &str = "conductor.ci.request";
/// NATS subject for CI results.
pub const CI_RESULT_SUBJECT: &str = "conductor.ci.result";

// ── Conversion from ci_runner types ──────────────────────────────────────────

impl CiResult {
    /// Convert a `CiCheckSuite` into a `CiResult` for NATS publication.
    pub fn from_suite(proposal_id: &str, branch: &str, suite: &CiCheckSuite) -> Self {
        let checks: Vec<CiCheckResult> = suite
            .checks
            .iter()
            .map(|c| CiCheckResult {
                check_type: c.check_type.to_string(),
                passed: c.passed,
                summary: c.summary.clone(),
                duration_secs: c.duration.as_secs_f64(),
            })
            .collect();

        CiResult {
            proposal_id: proposal_id.to_string(),
            branch: branch.to_string(),
            passed: suite.passed,
            checks,
            duration_secs: suite.total_duration.as_secs_f64(),
            error: suite.error.clone(),
            summary: suite.summary(),
        }
    }

    /// Extract test pass/fail counts from the check results.
    pub fn test_counts(&self) -> (u32, u32) {
        for check in &self.checks {
            if check.check_type == "test" {
                return parse_test_counts(&check.summary);
            }
        }
        (0, 0)
    }

    /// Extract clippy warning count.
    pub fn clippy_warnings(&self) -> u32 {
        for check in &self.checks {
            if check.check_type == "clippy" {
                return parse_warning_count(&check.summary);
            }
        }
        0
    }

    /// Whether fmt is clean.
    pub fn fmt_clean(&self) -> bool {
        self.checks
            .iter()
            .find(|c| c.check_type == "fmt")
            .map(|c| c.passed)
            .unwrap_or(true)
    }
}

// ── Branch checkout helper ───────────────────────────────────────────────────

/// Fetch and checkout a branch in the given repository.
///
/// Runs `git fetch origin <branch>` then `git checkout <branch>`.
/// Returns an error message if the checkout fails.
pub fn checkout_branch(repo_root: &Path, branch: &str) -> Result<(), String> {
    // Fetch the branch
    let fetch = Command::new("git")
        .args(["fetch", "origin", branch])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("failed to run git fetch: {e}"))?;

    if !fetch.status.success() {
        let stderr = String::from_utf8_lossy(&fetch.stderr);
        return Err(format!("git fetch failed: {stderr}"));
    }

    // Checkout the branch
    let checkout = Command::new("git")
        .args(["checkout", branch])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("failed to run git checkout: {e}"))?;

    if !checkout.status.success() {
        let stderr = String::from_utf8_lossy(&checkout.stderr);
        return Err(format!("git checkout failed: {stderr}"));
    }

    Ok(())
}

/// Run CI checks for a request: checkout branch, run checks, return result.
///
/// This is the core handler for a CI request. It:
/// 1. Determines the repo root
/// 2. Checks out the requested branch
/// 3. Runs cargo test, clippy, and fmt
/// 4. Returns a structured result
pub fn handle_ci_request(request: &CiRequest, default_repo_root: &Path) -> CiResult {
    let repo_root = request
        .repo_root
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_repo_root.to_path_buf());

    // Checkout the branch
    if let Err(e) = checkout_branch(&repo_root, &request.branch) {
        return CiResult {
            proposal_id: request.proposal_id.clone(),
            branch: request.branch.clone(),
            passed: false,
            checks: vec![],
            duration_secs: 0.0,
            error: Some(format!("Branch checkout failed: {e}")),
            summary: format!("CI error: branch checkout failed: {e}"),
        };
    }

    // Run CI checks
    let suite = crate::ci_runner::run_ci_checks(&repo_root);
    CiResult::from_suite(&request.proposal_id, &request.branch, &suite)
}

/// Default connection timeout for CI service.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// CI event stream configuration for JetStream durable events.
fn ci_stream_config() -> StreamConfig {
    StreamConfig::new("CONDUCTOR_EVENTS", vec!["ci.event.>".to_string()])
}

/// Start the CI service event loop.
///
/// Connects to NATS via noesis-ship [`ConnectionManager`], subscribes to
/// `conductor.ci.request`, and processes requests. Results are:
/// 1. Published to `conductor.ci.result` (for backward compatibility)
/// 2. Emitted to `CONDUCTOR_EVENTS` JetStream stream (durable, replayable)
/// 3. Sent as reply to the original request (for `nk pr recheck`)
///
/// This function runs forever (or until the NATS connection drops).
pub async fn run_ci_service(nats_url: &str, repo_root: PathBuf) -> Result<(), CiServiceError> {
    // Connect via noesis-ship ConnectionManager (consistent with reader.rs)
    let config = NatsConfig::new(nats_url).with_connect_timeout(CONNECT_TIMEOUT);
    let mut conn = ConnectionManager::new(config.clone());
    conn.connect()
        .await
        .map_err(|e| CiServiceError::Connection(e.to_string()))?;

    let client = conn
        .client()
        .map_err(|e| CiServiceError::Connection(e.to_string()))?
        .clone();

    // Set up EventBus for durable CI result events
    let event_bus =
        EventBus::with_stream(config, ci_stream_config(), "ci.event").with_source("conductor-ci");
    if let Err(e) = event_bus.connect().await {
        eprintln!("ci-service: EventBus connect failed (non-fatal): {e}");
        // Non-fatal: CI service works without EventBus (backward compat)
    }

    let mut subscriber = client
        .subscribe(CI_REQUEST_SUBJECT)
        .await
        .map_err(|e| CiServiceError::Subscribe(e.to_string()))?;

    while let Some(msg) = subscriber.next().await {
        let request: CiRequest = match serde_json::from_slice(&msg.payload) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ci-service: invalid request: {e}");
                continue;
            }
        };

        eprintln!(
            "ci-service: running checks for {} on branch {}",
            request.proposal_id, request.branch
        );

        let result = handle_ci_request(&request, &repo_root);

        let result_json = match serde_json::to_vec(&result) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("ci-service: failed to serialize result: {e}");
                continue;
            }
        };

        // 1. Publish to result subject (backward compatible)
        if let Err(e) = client
            .publish(CI_RESULT_SUBJECT, result_json.clone().into())
            .await
        {
            eprintln!("ci-service: failed to publish result: {e}");
        }

        // 2. Emit to EventBus (JetStream durable — enables CI dashboard, retry)
        let event_type = if result.passed { "completed" } else { "failed" };
        if let Err(e) = event_bus
            .emit(
                event_type,
                "conductor-ci",
                serde_json::to_value(&result).unwrap_or_default(),
                Some(&request.proposal_id),
            )
            .await
        {
            eprintln!("ci-service: EventBus emit failed (non-fatal): {e}");
        }

        // 3. Reply to original request (for nk pr recheck)
        if let Some(reply) = msg.reply
            && let Err(e) = client.publish(reply, result_json.into()).await
        {
            eprintln!("ci-service: failed to reply: {e}");
        }

        let status = if result.passed { "PASSED" } else { "FAILED" };
        eprintln!(
            "ci-service: {} {} ({:.1}s)",
            request.proposal_id, status, result.duration_secs
        );
    }

    Ok(())
}

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum CiServiceError {
    #[error("Failed to subscribe to CI requests: {0}")]
    Subscribe(String),

    #[error("NATS connection error: {0}")]
    Connection(String),
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse "X passed, Y failed" from a test summary.
fn parse_test_counts(summary: &str) -> (u32, u32) {
    let mut passed = 0u32;
    let mut failed = 0u32;

    for part in summary.split(',') {
        let part = part.trim();
        if let Some(num_str) = part.strip_suffix(" passed") {
            passed = num_str.trim().parse().unwrap_or(0);
        } else if let Some(num_str) = part.strip_suffix(" failed") {
            failed = num_str.trim().parse().unwrap_or(0);
        }
    }

    (passed, failed)
}

/// Parse "N warning(s)" from a clippy summary.
fn parse_warning_count(summary: &str) -> u32 {
    if summary == "no warnings" {
        return 0;
    }
    // "3 warning(s)" → 3
    summary
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_runner::{CheckResult, CheckType, CiCheckSuite};
    use std::time::Duration;

    #[test]
    fn test_ci_request_deserialize() {
        let json = r#"{"proposal_id": "PROP-2020", "branch": "expedition/ex-3077-nats-ci"}"#;
        let req: CiRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.proposal_id, "PROP-2020");
        assert_eq!(req.branch, "expedition/ex-3077-nats-ci");
        assert!(req.repo_root.is_none());
    }

    #[test]
    fn test_ci_request_with_repo_root() {
        let json = r#"{
            "proposal_id": "PROP-2020",
            "branch": "main",
            "repo_root": "/tmp/repo"
        }"#;
        let req: CiRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.repo_root, Some("/tmp/repo".to_string()));
    }

    #[test]
    fn test_ci_result_from_suite_all_pass() {
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
                    duration: Duration::from_secs(3),
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
            total_duration: Duration::from_secs(9),
            error: None,
        };

        let result = CiResult::from_suite("PROP-2020", "main", &suite);
        assert!(result.passed);
        assert_eq!(result.proposal_id, "PROP-2020");
        assert_eq!(result.branch, "main");
        assert_eq!(result.checks.len(), 3);
        assert!(result.summary.contains("PASSED"));
    }

    #[test]
    fn test_ci_result_from_suite_test_failure() {
        let suite = CiCheckSuite {
            checks: vec![
                CheckResult {
                    check_type: CheckType::Test,
                    passed: false,
                    summary: "10 passed, 3 failed".to_string(),
                    output: String::new(),
                    duration: Duration::from_secs(8),
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
            passed: false,
            total_duration: Duration::from_secs(11),
            error: None,
        };

        let result = CiResult::from_suite("PROP-2021", "feature/x", &suite);
        assert!(!result.passed);
        assert!(result.summary.contains("FAILED"));
    }

    #[test]
    fn test_ci_result_from_suite_error() {
        let suite = CiCheckSuite {
            checks: vec![],
            passed: false,
            total_duration: Duration::ZERO,
            error: Some("directory not found".to_string()),
        };

        let result = CiResult::from_suite("PROP-2022", "main", &suite);
        assert!(!result.passed);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_ci_result_test_counts() {
        let result = CiResult {
            proposal_id: "PROP-2020".to_string(),
            branch: "main".to_string(),
            passed: true,
            checks: vec![CiCheckResult {
                check_type: "test".to_string(),
                passed: true,
                summary: "42 passed, 1 failed".to_string(),
                duration_secs: 5.0,
            }],
            duration_secs: 5.0,
            error: None,
            summary: String::new(),
        };

        let (passed, failed) = result.test_counts();
        assert_eq!(passed, 42);
        assert_eq!(failed, 1);
    }

    #[test]
    fn test_ci_result_clippy_warnings() {
        let result = CiResult {
            proposal_id: "PROP-2020".to_string(),
            branch: "main".to_string(),
            passed: false,
            checks: vec![CiCheckResult {
                check_type: "clippy".to_string(),
                passed: false,
                summary: "5 warning(s)".to_string(),
                duration_secs: 3.0,
            }],
            duration_secs: 3.0,
            error: None,
            summary: String::new(),
        };

        assert_eq!(result.clippy_warnings(), 5);
    }

    #[test]
    fn test_ci_result_fmt_clean() {
        let result = CiResult {
            proposal_id: "PROP-2020".to_string(),
            branch: "main".to_string(),
            passed: true,
            checks: vec![CiCheckResult {
                check_type: "fmt".to_string(),
                passed: true,
                summary: "clean".to_string(),
                duration_secs: 1.0,
            }],
            duration_secs: 1.0,
            error: None,
            summary: String::new(),
        };

        assert!(result.fmt_clean());
    }

    #[test]
    fn test_ci_result_serialization_roundtrip() {
        let result = CiResult {
            proposal_id: "PROP-2020".to_string(),
            branch: "expedition/ex-3077".to_string(),
            passed: true,
            checks: vec![
                CiCheckResult {
                    check_type: "test".to_string(),
                    passed: true,
                    summary: "42 passed".to_string(),
                    duration_secs: 5.0,
                },
                CiCheckResult {
                    check_type: "clippy".to_string(),
                    passed: true,
                    summary: "no warnings".to_string(),
                    duration_secs: 3.0,
                },
            ],
            duration_secs: 8.0,
            error: None,
            summary: "CI PASSED (8.0s)".to_string(),
        };

        let json = serde_json::to_string(&result).expect("serialize");
        let deserialized: CiResult = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.proposal_id, result.proposal_id);
        assert_eq!(deserialized.branch, result.branch);
        assert_eq!(deserialized.passed, result.passed);
        assert_eq!(deserialized.checks.len(), result.checks.len());
        assert_eq!(deserialized.duration_secs, result.duration_secs);
    }

    #[test]
    fn test_parse_test_counts() {
        assert_eq!(parse_test_counts("42 passed"), (42, 0));
        assert_eq!(parse_test_counts("42 passed, 3 failed"), (42, 3));
        assert_eq!(parse_test_counts("0 passed, 0 failed"), (0, 0));
        assert_eq!(parse_test_counts("all tests passed"), (0, 0));
    }

    #[test]
    fn test_parse_warning_count() {
        assert_eq!(parse_warning_count("no warnings"), 0);
        assert_eq!(parse_warning_count("5 warning(s)"), 5);
        assert_eq!(parse_warning_count("1 warning(s)"), 1);
    }

    #[test]
    fn test_handle_ci_request_nonexistent_repo() {
        let request = CiRequest {
            proposal_id: "PROP-2020".to_string(),
            branch: "main".to_string(),
            repo_root: Some("/nonexistent/path".to_string()),
        };

        let result = handle_ci_request(&request, Path::new("/tmp"));
        assert!(!result.passed);
        // Should fail at checkout or at CI check
        assert!(result.error.is_some() || result.checks.is_empty());
    }

    #[test]
    fn test_ci_result_no_checks_returns_zero_counts() {
        let result = CiResult {
            proposal_id: "PROP-2020".to_string(),
            branch: "main".to_string(),
            passed: false,
            checks: vec![],
            duration_secs: 0.0,
            error: Some("no checks".to_string()),
            summary: String::new(),
        };

        assert_eq!(result.test_counts(), (0, 0));
        assert_eq!(result.clippy_warnings(), 0);
        assert!(result.fmt_clean()); // no fmt check → assume clean
    }
}
