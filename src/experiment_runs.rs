//! Experiment run tracking for the HDD (research) board.
//!
//! Tracks timestamped executions of research experiments. Each experiment
//! (EXPR-XXX) can have multiple runs (different versions, parameters, retries).
//!
//! Stored as Arrow RecordBatches, persisted to `experiment_runs.parquet`.
//!
//! # CLI Usage
//!
//! ```text
//! nk hdd experiment run EXPR-131.1           # Start a new run
//! nk hdd experiment status EXPR-131.1        # Show all runs
//! nk hdd experiment complete EXPR-131.1 --run 1 --results '{"accuracy": 0.85}'
//! ```

use crate::schema::{experiment_runs_schema, expr_run_col};
use arrow::array::{Array, RecordBatch, StringArray, TimestampMillisecondArray, UInt32Array};
use std::sync::Arc;

/// Errors from experiment run operations.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Experiment not found: {0}")]
    ExperimentNotFound(String),

    #[error("Run not found: {0} run #{1}")]
    RunNotFound(String, u32),

    #[error("Run already complete: {0} run #{1}")]
    AlreadyComplete(String, u32),
}

/// A single experiment run for display.
#[derive(Debug, Clone)]
pub struct ExperimentRun {
    pub run_id: String,
    pub experiment_id: String,
    pub run_number: u32,
    pub status: String,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub results_json: Option<String>,
    pub agent: Option<String>,
}

/// Store for experiment runs, backed by Arrow RecordBatches.
pub struct ExperimentRunStore {
    batches: Vec<RecordBatch>,
}

impl ExperimentRunStore {
    pub fn new() -> Self {
        Self {
            batches: Vec::new(),
        }
    }

    /// Load from existing batches (e.g., from Parquet).
    pub fn from_batches(batches: Vec<RecordBatch>) -> Self {
        Self { batches }
    }

    /// Get all batches (for persistence).
    pub fn batches(&self) -> &[RecordBatch] {
        &self.batches
    }

    /// Start a new run for an experiment. Returns the run ID.
    pub fn start_run(
        &mut self,
        experiment_id: &str,
        agent: Option<&str>,
    ) -> Result<String, RunError> {
        let run_number = self.next_run_number(experiment_id);
        let run_id = format!("RUN-{experiment_id}-{run_number:03}");
        let now_ms = chrono::Utc::now().timestamp_millis();

        let schema = experiment_runs_schema();
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec![run_id.as_str()])),
                Arc::new(StringArray::from(vec![experiment_id])),
                Arc::new(UInt32Array::from(vec![run_number])),
                Arc::new(StringArray::from(vec!["running"])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(TimestampMillisecondArray::from(vec![None::<i64>]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![None::<&str>])),
                Arc::new(StringArray::from(vec![agent])),
            ],
        )?;

        self.batches.push(batch);
        Ok(run_id)
    }

    /// Complete a run with results.
    pub fn complete_run(
        &mut self,
        experiment_id: &str,
        run_number: u32,
        results_json: Option<&str>,
    ) -> Result<(), RunError> {
        let now_ms = chrono::Utc::now().timestamp_millis();

        for batch in &mut self.batches {
            let ids = batch
                .column(expr_run_col::EXPERIMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("experiment_id column");
            let nums = batch
                .column(expr_run_col::RUN_NUMBER)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .expect("run_number column");
            let statuses = batch
                .column(expr_run_col::STATUS)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("status column");

            for i in 0..batch.num_rows() {
                if ids.value(i) == experiment_id && nums.value(i) == run_number {
                    if statuses.value(i) == "complete" || statuses.value(i) == "failed" {
                        return Err(RunError::AlreadyComplete(
                            experiment_id.to_string(),
                            run_number,
                        ));
                    }

                    // Rebuild batch with updated status, completed_at, results_json
                    let schema = experiment_runs_schema();
                    let mut columns: Vec<Arc<dyn Array>> = Vec::new();

                    for c in 0..batch.num_columns() {
                        match c {
                            c if c == expr_run_col::STATUS => {
                                let mut vals: Vec<&str> = Vec::new();
                                let col = batch
                                    .column(c)
                                    .as_any()
                                    .downcast_ref::<StringArray>()
                                    .expect("status");
                                for j in 0..col.len() {
                                    if j == i {
                                        vals.push("complete");
                                    } else {
                                        vals.push(col.value(j));
                                    }
                                }
                                columns.push(Arc::new(StringArray::from(vals)));
                            }
                            c if c == expr_run_col::COMPLETED_AT => {
                                let col = batch
                                    .column(c)
                                    .as_any()
                                    .downcast_ref::<TimestampMillisecondArray>()
                                    .expect("completed_at");
                                let mut vals: Vec<Option<i64>> = Vec::new();
                                for j in 0..col.len() {
                                    if j == i {
                                        vals.push(Some(now_ms));
                                    } else if col.is_null(j) {
                                        vals.push(None);
                                    } else {
                                        vals.push(Some(col.value(j)));
                                    }
                                }
                                columns.push(Arc::new(
                                    TimestampMillisecondArray::from(vals).with_timezone("UTC"),
                                ));
                            }
                            c if c == expr_run_col::RESULTS_JSON => {
                                let col = batch
                                    .column(c)
                                    .as_any()
                                    .downcast_ref::<StringArray>()
                                    .expect("results_json");
                                let mut vals: Vec<Option<&str>> = Vec::new();
                                for j in 0..col.len() {
                                    if j == i {
                                        vals.push(results_json);
                                    } else if col.is_null(j) {
                                        vals.push(None);
                                    } else {
                                        vals.push(Some(col.value(j)));
                                    }
                                }
                                columns.push(Arc::new(StringArray::from(vals)));
                            }
                            _ => {
                                columns.push(batch.column(c).clone());
                            }
                        }
                    }

                    *batch = RecordBatch::try_new(schema, columns)?;
                    return Ok(());
                }
            }
        }

        Err(RunError::RunNotFound(experiment_id.to_string(), run_number))
    }

    /// List all runs for an experiment, ordered by run number.
    pub fn list_runs(&self, experiment_id: &str) -> Vec<ExperimentRun> {
        let mut runs = Vec::new();

        for batch in &self.batches {
            let ids = batch
                .column(expr_run_col::EXPERIMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("experiment_id column");

            for i in 0..batch.num_rows() {
                if ids.value(i) == experiment_id {
                    runs.push(extract_run(batch, i));
                }
            }
        }

        runs.sort_by_key(|r| r.run_number);
        runs
    }

    /// Get the next run number for an experiment.
    fn next_run_number(&self, experiment_id: &str) -> u32 {
        let mut max = 0u32;
        for batch in &self.batches {
            let ids = batch
                .column(expr_run_col::EXPERIMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("experiment_id column");
            let nums = batch
                .column(expr_run_col::RUN_NUMBER)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .expect("run_number column");

            for i in 0..batch.num_rows() {
                if ids.value(i) == experiment_id && nums.value(i) > max {
                    max = nums.value(i);
                }
            }
        }
        max + 1
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.batches.iter().all(|b| b.num_rows() == 0)
    }
}

impl Default for ExperimentRunStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Format runs for display.
pub fn format_runs(runs: &[ExperimentRun]) -> String {
    if runs.is_empty() {
        return "No runs recorded.\n".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "  {:<25} {:<8} {:<10} {:<24} {}",
        "Run ID", "Run #", "Status", "Started", "Results"
    ));
    lines.push(format!("  {}", "-".repeat(80)));

    for run in runs {
        let started = chrono::DateTime::from_timestamp_millis(run.started_at)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "?".to_string());

        let results = run
            .results_json
            .as_deref()
            .unwrap_or("—")
            .chars()
            .take(30)
            .collect::<String>();

        lines.push(format!(
            "  {:<25} {:<8} {:<10} {:<24} {}",
            run.run_id, run.run_number, run.status, started, results
        ));
    }

    lines.join("\n") + "\n"
}

fn extract_run(batch: &RecordBatch, idx: usize) -> ExperimentRun {
    let run_ids = batch
        .column(expr_run_col::RUN_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("run_id");
    let expr_ids = batch
        .column(expr_run_col::EXPERIMENT_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("experiment_id");
    let nums = batch
        .column(expr_run_col::RUN_NUMBER)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .expect("run_number");
    let statuses = batch
        .column(expr_run_col::STATUS)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("status");
    let started = batch
        .column(expr_run_col::STARTED_AT)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .expect("started_at");
    let completed = batch
        .column(expr_run_col::COMPLETED_AT)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .expect("completed_at");
    let results = batch
        .column(expr_run_col::RESULTS_JSON)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("results_json");
    let agents = batch
        .column(expr_run_col::AGENT)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("agent");

    ExperimentRun {
        run_id: run_ids.value(idx).to_string(),
        experiment_id: expr_ids.value(idx).to_string(),
        run_number: nums.value(idx),
        status: statuses.value(idx).to_string(),
        started_at: started.value(idx),
        completed_at: if completed.is_null(idx) {
            None
        } else {
            Some(completed.value(idx))
        },
        results_json: if results.is_null(idx) {
            None
        } else {
            Some(results.value(idx).to_string())
        },
        agent: if agents.is_null(idx) {
            None
        } else {
            Some(agents.value(idx).to_string())
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_start_run() {
        let mut store = ExperimentRunStore::new();
        let run_id = store.start_run("EXPR-131.1", Some("DGX")).unwrap();
        assert!(run_id.starts_with("RUN-EXPR-131.1-"));
        assert!(run_id.contains("001"));

        let runs = store.list_runs("EXPR-131.1");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].run_number, 1);
        assert_eq!(runs[0].agent, Some("DGX".to_string()));
    }

    #[test]
    fn test_multiple_runs_increment() {
        let mut store = ExperimentRunStore::new();
        store.start_run("EXPR-131.1", None).unwrap();
        store.start_run("EXPR-131.1", None).unwrap();
        let id3 = store.start_run("EXPR-131.1", None).unwrap();

        assert!(id3.contains("003"));
        let runs = store.list_runs("EXPR-131.1");
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[2].run_number, 3);
    }

    #[test]
    fn test_complete_run() {
        let mut store = ExperimentRunStore::new();
        store.start_run("EXPR-131.1", None).unwrap();

        store
            .complete_run("EXPR-131.1", 1, Some(r#"{"accuracy": 0.85}"#))
            .unwrap();

        let runs = store.list_runs("EXPR-131.1");
        assert_eq!(runs[0].status, "complete");
        assert!(runs[0].completed_at.is_some());
        assert_eq!(
            runs[0].results_json,
            Some(r#"{"accuracy": 0.85}"#.to_string())
        );
    }

    #[test]
    fn test_complete_already_complete_errors() {
        let mut store = ExperimentRunStore::new();
        store.start_run("EXPR-131.1", None).unwrap();
        store.complete_run("EXPR-131.1", 1, None).unwrap();

        let result = store.complete_run("EXPR-131.1", 1, None);
        assert!(matches!(result, Err(RunError::AlreadyComplete(_, _))));
    }

    #[test]
    fn test_complete_nonexistent_run_errors() {
        let mut store = ExperimentRunStore::new();
        let result = store.complete_run("EXPR-999", 1, None);
        assert!(matches!(result, Err(RunError::RunNotFound(_, _))));
    }

    #[test]
    fn test_isolation_between_experiments() {
        let mut store = ExperimentRunStore::new();
        store.start_run("EXPR-131.1", None).unwrap();
        store.start_run("EXPR-131.2", None).unwrap();

        assert_eq!(store.list_runs("EXPR-131.1").len(), 1);
        assert_eq!(store.list_runs("EXPR-131.2").len(), 1);
        assert_eq!(store.list_runs("EXPR-999").len(), 0);
    }

    #[test]
    fn test_format_runs_empty() {
        let output = format_runs(&[]);
        assert!(output.contains("No runs"));
    }

    #[test]
    fn test_format_runs_display() {
        let mut store = ExperimentRunStore::new();
        store.start_run("EXPR-131.1", Some("DGX")).unwrap();
        let runs = store.list_runs("EXPR-131.1");
        let output = format_runs(&runs);
        assert!(output.contains("RUN-EXPR-131.1-001"));
        assert!(output.contains("running"));
    }

    #[test]
    fn test_runs_ordered_by_number() {
        let mut store = ExperimentRunStore::new();
        store.start_run("EXPR-1", None).unwrap();
        store.start_run("EXPR-1", None).unwrap();
        store.start_run("EXPR-1", None).unwrap();

        let runs = store.list_runs("EXPR-1");
        assert_eq!(runs[0].run_number, 1);
        assert_eq!(runs[1].run_number, 2);
        assert_eq!(runs[2].run_number, 3);
    }
}
