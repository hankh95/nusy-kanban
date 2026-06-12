//! CI result storage — Arrow-backed store for CI check results.
//!
//! Stores `cargo test`, `cargo clippy`, and `cargo fmt` results per proposal,
//! queryable by proposal ID. Follows the same pattern as [`CommentStore`].

use arrow::array::{
    Array, BooleanArray, Float64Array, RecordBatch, StringArray, TimestampMillisecondArray,
    UInt32Array,
};
use arrow::datatypes::Schema;
use std::sync::Arc;

use crate::schema::{ci_results_col, ci_results_schema};

// ── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum CiStoreError {
    #[error("CI result not found: {0}")]
    NotFound(String),

    #[error("Internal error: {0}")]
    InternalError(String),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

pub type Result<T> = std::result::Result<T, CiStoreError>;

/// Status of a CI run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiStatus {
    Pending,
    Passed,
    Failed,
    Error,
}

impl CiStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CiStatus::Pending => "pending",
            CiStatus::Passed => "passed",
            CiStatus::Failed => "failed",
            CiStatus::Error => "error",
        }
    }
}

impl std::str::FromStr for CiStatus {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "passed" => CiStatus::Passed,
            "failed" => CiStatus::Failed,
            "error" => CiStatus::Error,
            _ => CiStatus::Pending,
        })
    }
}

impl std::fmt::Display for CiStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Input for recording a CI result.
pub struct CiResultInput<'a> {
    pub proposal_id: &'a str,
    pub status: CiStatus,
    pub test_passed: u32,
    pub test_failed: u32,
    pub clippy_warnings: u32,
    pub fmt_clean: bool,
    pub duration_secs: f64,
    pub error_message: Option<&'a str>,
    pub summary: &'a str,
}

// ── CiResultStore ───────────────────────────────────────────────────────────

pub struct CiResultStore {
    batches: Vec<RecordBatch>,
    schema: Arc<Schema>,
}

impl CiResultStore {
    pub fn new() -> Self {
        Self {
            batches: Vec::new(),
            schema: ci_results_schema(),
        }
    }

    pub fn ci_batches(&self) -> &[RecordBatch] {
        &self.batches
    }

    pub fn ci_schema(&self) -> &Arc<Schema> {
        &self.schema
    }

    pub fn load_results(&mut self, batches: Vec<RecordBatch>) {
        self.batches = batches;
    }

    /// Record a CI result for a proposal. Replaces any existing result for that proposal.
    pub fn record_result(&mut self, input: &CiResultInput<'_>) -> Result<String> {
        // Remove any existing result for this proposal
        self.remove_for_proposal(input.proposal_id);

        let run_id = format!("CI-{:04}", self.total_count() + 1);
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![run_id.as_str()])),
                Arc::new(StringArray::from(vec![input.proposal_id])),
                Arc::new(StringArray::from(vec![input.status.as_str()])),
                Arc::new(UInt32Array::from(vec![input.test_passed])),
                Arc::new(UInt32Array::from(vec![input.test_failed])),
                Arc::new(UInt32Array::from(vec![input.clippy_warnings])),
                Arc::new(BooleanArray::from(vec![input.fmt_clean])),
                Arc::new(Float64Array::from(vec![input.duration_secs])),
                Arc::new(StringArray::from(vec![input.error_message])),
                Arc::new(StringArray::from(vec![input.summary])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
            ],
        )?;

        self.batches.push(batch);
        Ok(run_id)
    }

    /// Get the latest CI result for a proposal.
    pub fn get_result(&self, proposal_id: &str) -> Result<Option<CiResultView>> {
        // Walk backwards to find the latest result
        for batch in self.batches.iter().rev() {
            let prop_ids = batch
                .column(ci_results_col::PROPOSAL_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| CiStoreError::InternalError("proposal_id downcast".into()))?;

            for i in (0..batch.num_rows()).rev() {
                if prop_ids.value(i) == proposal_id {
                    return Ok(Some(self.extract_view(batch, i)?));
                }
            }
        }
        Ok(None)
    }

    /// Remove all results for a proposal.
    fn remove_for_proposal(&mut self, proposal_id: &str) {
        self.batches.retain(|batch| {
            let prop_ids = batch
                .column(ci_results_col::PROPOSAL_ID)
                .as_any()
                .downcast_ref::<StringArray>();
            match prop_ids {
                Some(ids) => {
                    // Keep batches that DON'T match (or multi-row batches we can't filter easily)
                    if batch.num_rows() == 1 {
                        ids.value(0) != proposal_id
                    } else {
                        true // Keep multi-row batches (we always write single-row)
                    }
                }
                None => true,
            }
        });
    }

    fn total_count(&self) -> usize {
        self.batches.iter().map(|b| b.num_rows()).sum()
    }

    fn extract_view(&self, batch: &RecordBatch, row: usize) -> Result<CiResultView> {
        let get_str = |col: usize| -> Result<String> {
            batch
                .column(col)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| CiStoreError::InternalError(format!("col {col} downcast")))
                .map(|a| a.value(row).to_string())
        };

        let get_u32 = |col: usize| -> Result<u32> {
            batch
                .column(col)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .ok_or_else(|| CiStoreError::InternalError(format!("col {col} downcast")))
                .map(|a| a.value(row))
        };

        let run_id = get_str(ci_results_col::RUN_ID)?;
        let status_str = get_str(ci_results_col::STATUS)?;
        let test_passed = get_u32(ci_results_col::TEST_PASSED)?;
        let test_failed = get_u32(ci_results_col::TEST_FAILED)?;
        let clippy_warnings = get_u32(ci_results_col::CLIPPY_WARNINGS)?;

        let fmt_clean = batch
            .column(ci_results_col::FMT_CLEAN)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .ok_or_else(|| CiStoreError::InternalError("fmt_clean downcast".into()))?
            .value(row);

        let duration_secs = batch
            .column(ci_results_col::DURATION_SECS)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or_else(|| CiStoreError::InternalError("duration downcast".into()))?
            .value(row);

        let error_col = batch
            .column(ci_results_col::ERROR_MESSAGE)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| CiStoreError::InternalError("error_message downcast".into()))?;
        let error_message = if error_col.is_null(row) {
            None
        } else {
            Some(error_col.value(row).to_string())
        };

        let summary = get_str(ci_results_col::SUMMARY)?;

        let completed_at = batch
            .column(ci_results_col::COMPLETED_AT)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .ok_or_else(|| CiStoreError::InternalError("completed_at downcast".into()))?
            .value(row);

        Ok(CiResultView {
            run_id,
            status: status_str.parse::<CiStatus>().unwrap_or(CiStatus::Pending),
            test_passed,
            test_failed,
            clippy_warnings,
            fmt_clean,
            duration_secs,
            error_message,
            summary,
            completed_at_ms: completed_at,
        })
    }
}

impl Default for CiResultStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Read-only view of a CI result.
#[derive(Debug, Clone)]
pub struct CiResultView {
    pub run_id: String,
    pub status: CiStatus,
    pub test_passed: u32,
    pub test_failed: u32,
    pub clippy_warnings: u32,
    pub fmt_clean: bool,
    pub duration_secs: f64,
    pub error_message: Option<String>,
    pub summary: String,
    pub completed_at_ms: i64,
}

impl CiResultView {
    /// Format for `nk pr checks` display.
    pub fn format_checks(&self) -> String {
        let mut out = String::new();

        let status_label = match self.status {
            CiStatus::Passed => "PASSED",
            CiStatus::Failed => "FAILED",
            CiStatus::Error => "ERROR",
            CiStatus::Pending => "PENDING",
        };

        out.push_str(&format!("CI Status: {status_label}\n"));
        out.push_str(&format!("  Run: {}\n", self.run_id));
        out.push_str(&format!("  Duration: {:.1}s\n\n", self.duration_secs));

        // Test results
        let test_icon = if self.test_failed == 0 { "✓" } else { "✗" };
        out.push_str(&format!(
            "  {test_icon} Tests: {} passed, {} failed\n",
            self.test_passed, self.test_failed
        ));

        // Clippy
        let clippy_icon = if self.clippy_warnings == 0 {
            "✓"
        } else {
            "✗"
        };
        out.push_str(&format!(
            "  {clippy_icon} Clippy: {} warning(s)\n",
            self.clippy_warnings
        ));

        // Fmt
        let fmt_icon = if self.fmt_clean { "✓" } else { "✗" };
        let fmt_label = if self.fmt_clean {
            "clean"
        } else {
            "needs formatting"
        };
        out.push_str(&format!("  {fmt_icon} Format: {fmt_label}\n"));

        if let Some(ref err) = self.error_message {
            out.push_str(&format!("\n  Error: {err}\n"));
        }

        out
    }
}

// ── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_get_result() {
        let mut store = CiResultStore::new();
        let run_id = store
            .record_result(&CiResultInput {
                proposal_id: "PROP-2001",
                status: CiStatus::Passed,
                test_passed: 42,
                test_failed: 0,
                clippy_warnings: 0,
                fmt_clean: true,
                duration_secs: 8.5,
                error_message: None,
                summary: "42 passed, 0 failed",
            })
            .expect("record");
        assert_eq!(run_id, "CI-0001");

        let view = store.get_result("PROP-2001").expect("get").expect("found");
        assert_eq!(view.status, CiStatus::Passed);
        assert_eq!(view.test_passed, 42);
        assert_eq!(view.test_failed, 0);
        assert!(view.fmt_clean);
    }

    #[test]
    fn test_record_replaces_previous() {
        let mut store = CiResultStore::new();

        store
            .record_result(&CiResultInput {
                proposal_id: "PROP-2001",
                status: CiStatus::Failed,
                test_passed: 10,
                test_failed: 2,
                clippy_warnings: 0,
                fmt_clean: true,
                duration_secs: 5.0,
                error_message: None,
                summary: "10 passed, 2 failed",
            })
            .expect("first");

        store
            .record_result(&CiResultInput {
                proposal_id: "PROP-2001",
                status: CiStatus::Passed,
                test_passed: 12,
                test_failed: 0,
                clippy_warnings: 0,
                fmt_clean: true,
                duration_secs: 6.0,
                error_message: None,
                summary: "12 passed, 0 failed",
            })
            .expect("second");

        let view = store.get_result("PROP-2001").expect("get").expect("found");
        assert_eq!(view.status, CiStatus::Passed);
        assert_eq!(view.test_passed, 12);
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let store = CiResultStore::new();
        let result = store.get_result("PROP-9999").expect("get");
        assert!(result.is_none());
    }

    #[test]
    fn test_ci_status_display() {
        assert_eq!(CiStatus::Passed.to_string(), "passed");
        assert_eq!(CiStatus::Failed.to_string(), "failed");
        assert_eq!(CiStatus::Error.to_string(), "error");
        assert_eq!(CiStatus::Pending.to_string(), "pending");
    }

    #[test]
    fn test_ci_status_from_str() {
        assert_eq!("passed".parse::<CiStatus>().unwrap(), CiStatus::Passed);
        assert_eq!("failed".parse::<CiStatus>().unwrap(), CiStatus::Failed);
        assert_eq!("unknown".parse::<CiStatus>().unwrap(), CiStatus::Pending);
    }

    #[test]
    fn test_format_checks_passed() {
        let view = CiResultView {
            run_id: "CI-0001".to_string(),
            status: CiStatus::Passed,
            test_passed: 42,
            test_failed: 0,
            clippy_warnings: 0,
            fmt_clean: true,
            duration_secs: 8.5,
            error_message: None,
            summary: "42 passed".to_string(),
            completed_at_ms: 0,
        };
        let out = view.format_checks();
        assert!(out.contains("PASSED"));
        assert!(out.contains("42 passed"));
        assert!(out.contains("0 warning"));
        assert!(out.contains("clean"));
    }

    #[test]
    fn test_format_checks_failed() {
        let view = CiResultView {
            run_id: "CI-0002".to_string(),
            status: CiStatus::Failed,
            test_passed: 10,
            test_failed: 3,
            clippy_warnings: 2,
            fmt_clean: false,
            duration_secs: 12.0,
            error_message: Some("clippy failed".to_string()),
            summary: "10 passed, 3 failed".to_string(),
            completed_at_ms: 0,
        };
        let out = view.format_checks();
        assert!(out.contains("FAILED"));
        assert!(out.contains("3 failed"));
        assert!(out.contains("2 warning"));
        assert!(out.contains("needs formatting"));
        assert!(out.contains("clippy failed"));
    }

    #[test]
    fn test_separate_proposals_independent() {
        let mut store = CiResultStore::new();

        store
            .record_result(&CiResultInput {
                proposal_id: "PROP-2001",
                status: CiStatus::Passed,
                test_passed: 10,
                test_failed: 0,
                clippy_warnings: 0,
                fmt_clean: true,
                duration_secs: 5.0,
                error_message: None,
                summary: "ok",
            })
            .expect("first");

        store
            .record_result(&CiResultInput {
                proposal_id: "PROP-2002",
                status: CiStatus::Failed,
                test_passed: 5,
                test_failed: 1,
                clippy_warnings: 0,
                fmt_clean: true,
                duration_secs: 3.0,
                error_message: None,
                summary: "fail",
            })
            .expect("second");

        let v1 = store.get_result("PROP-2001").expect("get").expect("found");
        assert_eq!(v1.status, CiStatus::Passed);

        let v2 = store.get_result("PROP-2002").expect("get").expect("found");
        assert_eq!(v2.status, CiStatus::Failed);
    }
}
