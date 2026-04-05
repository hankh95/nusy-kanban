//! Training job queue — coordinate GPU training runs across the fleet.
//!
//! EX-3332: Replaces research/TRAINING-QUEUE.md with a machine-readable queue.
//! CH-3338: Refactored to use noesis-ship's generic `JobQueue<TrainingPayload>`.
//!
//! ## Lifecycle
//!
//! ```text
//! queued → running → complete
//!                  → failed
//! ```

use noesis_ship::job_queue::{Job, JobQueue};
use serde::{Deserialize, Serialize};

// Re-export for callers that use JobStatus directly.
pub use noesis_ship::job_queue::JobStatus;

/// Training-specific job payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingPayload {
    pub experiment_id: String,
    pub being: String,
    pub corpus: String,
}

/// A training job (alias for the generic Job wrapper).
pub type TrainingJob = Job<TrainingPayload>;

/// Training queue — manages training job lifecycle.
///
/// Thin wrapper around `JobQueue<TrainingPayload>` preserving the original API.
/// The queue is serializable to JSON for file-based persistence.
#[derive(Debug, Serialize, Deserialize)]
pub struct TrainingQueue {
    inner: JobQueue<TrainingPayload>,
}

impl TrainingQueue {
    pub fn new() -> Self {
        Self {
            inner: JobQueue::new("TRAIN"),
        }
    }

    /// Queue a new training job. Returns the job ID.
    pub fn queue_job(
        &mut self,
        experiment_id: &str,
        being: &str,
        corpus: &str,
        machine: &str,
        queued_by: &str,
    ) -> String {
        self.inner.submit(
            TrainingPayload {
                experiment_id: experiment_id.to_string(),
                being: being.to_string(),
                corpus: corpus.to_string(),
            },
            machine,
            queued_by,
        )
    }

    /// Claim the next queued job for a machine.
    pub fn claim_job(&mut self, machine: &str) -> Option<&TrainingJob> {
        self.inner.claim(machine)
    }

    /// Complete a job with results path.
    pub fn complete_job(&mut self, job_id: &str, results_path: &str) -> bool {
        self.inner
            .complete(job_id, serde_json::json!({ "results_path": results_path }))
    }

    /// Fail a job with an error message.
    pub fn fail_job(&mut self, job_id: &str, error: &str) -> bool {
        self.inner.fail(job_id, error)
    }

    /// List jobs, optionally filtered by status.
    pub fn list_jobs(&self, status: Option<&JobStatus>) -> Vec<&TrainingJob> {
        self.inner.list(status)
    }

    /// Get a job by ID.
    pub fn get_job(&self, job_id: &str) -> Option<&TrainingJob> {
        self.inner.get(job_id)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Format the queue for display, optionally filtered.
    pub fn format_table_filtered(&self, status: Option<&JobStatus>) -> String {
        let jobs = self.list_jobs(status);
        self.format_jobs(&jobs)
    }

    /// Format the queue for display (all jobs).
    pub fn format_table(&self) -> String {
        let jobs = self.list_jobs(None);
        self.format_jobs(&jobs)
    }

    fn format_jobs(&self, jobs: &[&TrainingJob]) -> String {
        if jobs.is_empty() {
            return "No training jobs.\n".to_string();
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "  {:<12} {:<12} {:<25} {:<10} {:<8} {}",
            "Job ID", "Experiment", "Being", "Machine", "Status", "Queued By"
        ));
        lines.push(format!("  {}", "-".repeat(85)));

        for job in jobs {
            lines.push(format!(
                "  {:<12} {:<12} {:<25} {:<10} {:<8} {}",
                job.id,
                job.payload.experiment_id,
                truncate(&job.payload.being, 25),
                job.worker,
                job.status.as_str(),
                job.queued_by,
            ));
        }

        let (queued, running, complete, failed) = self.inner.counts();
        lines.push(format!(
            "\n  Total: {} jobs ({} queued, {} running, {} complete, {} failed)",
            jobs.len(),
            queued,
            running,
            complete,
            failed,
        ));

        lines.join("\n") + "\n"
    }
}

impl Default for TrainingQueue {
    fn default() -> Self {
        Self::new()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_job() {
        let mut q = TrainingQueue::new();
        let id = q.queue_job("EXPR-3275", "santiago-bahai", "bahai", "DGX", "Captain");
        assert_eq!(id, "TRAIN-001");
        assert_eq!(q.len(), 1);

        let job = q.get_job("TRAIN-001").expect("job");
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.payload.experiment_id, "EXPR-3275");
        assert_eq!(job.payload.being, "santiago-bahai");
    }

    #[test]
    fn test_claim_job() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-1", "being-1", "corpus-1", "DGX", "Captain");

        let job = q.claim_job("DGX").expect("claim");
        assert_eq!(job.status, JobStatus::Running);
        assert!(job.started_at.is_some());

        assert!(q.claim_job("DGX").is_none());
    }

    #[test]
    fn test_claim_filters_by_machine() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-1", "being-1", "corpus-1", "DGX", "Captain");
        q.queue_job("EXPR-2", "being-2", "corpus-2", "Mini", "Captain");

        let job = q.claim_job("Mini").expect("claim");
        assert_eq!(job.payload.experiment_id, "EXPR-2");

        let job = q.claim_job("DGX").expect("claim");
        assert_eq!(job.payload.experiment_id, "EXPR-1");
    }

    #[test]
    fn test_complete_job() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-1", "being", "corpus", "DGX", "Captain");
        q.claim_job("DGX");

        assert!(q.complete_job("TRAIN-001", "research/shared/eval-data/expr1/"));
        let job = q.get_job("TRAIN-001").unwrap();
        assert_eq!(job.status, JobStatus::Complete);
        assert!(job.completed_at.is_some());
    }

    #[test]
    fn test_fail_job() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-1", "being", "corpus", "DGX", "Captain");
        q.claim_job("DGX");

        assert!(q.fail_job("TRAIN-001", "OOM at epoch 3"));
        let job = q.get_job("TRAIN-001").unwrap();
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error.as_deref(), Some("OOM at epoch 3"));
    }

    #[test]
    fn test_cannot_complete_queued_job() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-1", "being", "corpus", "DGX", "Captain");
        assert!(!q.complete_job("TRAIN-001", "path"));
    }

    #[test]
    fn test_list_jobs_filter() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-1", "b1", "c1", "DGX", "Captain");
        q.queue_job("EXPR-2", "b2", "c2", "DGX", "Captain");
        q.claim_job("DGX");

        assert_eq!(q.list_jobs(Some(&JobStatus::Queued)).len(), 1);
        assert_eq!(q.list_jobs(Some(&JobStatus::Running)).len(), 1);
        assert_eq!(q.list_jobs(None).len(), 2);
    }

    #[test]
    fn test_job_ids_increment() {
        let mut q = TrainingQueue::new();
        assert_eq!(q.queue_job("A", "b", "c", "D", "E"), "TRAIN-001");
        assert_eq!(q.queue_job("A", "b", "c", "D", "E"), "TRAIN-002");
        assert_eq!(q.queue_job("A", "b", "c", "D", "E"), "TRAIN-003");
    }

    #[test]
    fn test_format_table() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-3275", "santiago-bahai", "bahai", "DGX", "Captain");
        let output = q.format_table();
        assert!(output.contains("TRAIN-001"));
        assert!(output.contains("EXPR-3275"));
        assert!(output.contains("queued"));
        assert!(output.contains("Total: 1 jobs"));
    }

    #[test]
    fn test_empty_format() {
        let q = TrainingQueue::new();
        assert!(q.format_table().contains("No training jobs"));
    }

    #[test]
    fn test_status_roundtrip() {
        for status in &[
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Complete,
            JobStatus::Failed,
        ] {
            let s = status.as_str();
            assert_eq!(&JobStatus::parse(s).expect("parse"), status);
        }
    }
}
