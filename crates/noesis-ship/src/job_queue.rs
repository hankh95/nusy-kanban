//! Generic job queue with lifecycle management.
//!
//! A job queue tracks work items through `queued → running → complete | failed`.
//! Workers claim jobs atomically (filtered by worker name), preventing
//! double-dispatch. Jobs carry a user-defined payload `J` alongside
//! queue metadata (status, timestamps, worker, result/error).
//!
//! Currently backed by an in-memory `HashMap`. Designed for future NATS KV
//! backing with compare-and-swap for distributed atomic claiming.
//!
//! # Example
//!
//! ```rust
//! use noesis_ship::job_queue::{JobQueue, JobStatus};
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Debug, Clone, Serialize, Deserialize)]
//! struct BuildJob {
//!     repo: String,
//!     branch: String,
//! }
//!
//! let mut queue = JobQueue::<BuildJob>::new("BUILD");
//!
//! let id = queue.submit(
//!     BuildJob { repo: "myapp".into(), branch: "main".into() },
//!     "ci-server",   // target worker
//!     "developer-1", // queued by
//! );
//!
//! let job = queue.claim("ci-server").unwrap();
//! let job_id = job.id.clone();
//! queue.complete(&job_id, serde_json::json!({"artifact": "build/out.tar"}));
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Job status in the queue lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Complete,
    Failed,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "complete" => Some(Self::Complete),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A job in the queue: metadata wrapper around a user-defined payload `J`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job<J> {
    /// Unique job ID (e.g., "BUILD-001").
    pub id: String,
    /// User-defined payload.
    pub payload: J,
    /// Current status.
    pub status: JobStatus,
    /// Target worker (jobs are claimed by matching worker name).
    pub worker: String,
    /// Who queued this job.
    pub queued_by: String,
    /// Timestamp (ms since epoch) when queued.
    pub queued_at: i64,
    /// Timestamp when claimed (running).
    pub started_at: Option<i64>,
    /// Timestamp when completed or failed.
    pub completed_at: Option<i64>,
    /// Result data (set on complete).
    pub result: Option<serde_json::Value>,
    /// Error message (set on fail).
    pub error: Option<String>,
}

/// Generic job queue with lifecycle management.
///
/// `J` is the user-defined job payload (must be `Serialize + Deserialize`).
/// The `prefix` is used for ID generation (e.g., prefix "BUILD" → "BUILD-001").
#[derive(Debug, Serialize, Deserialize)]
#[serde(bound = "J: Serialize + for<'a> Deserialize<'a>")]
pub struct JobQueue<J> {
    jobs: HashMap<String, Job<J>>,
    prefix: String,
    next_id: u32,
}

impl<J: Clone> JobQueue<J> {
    /// Create a new empty queue with the given ID prefix.
    pub fn new(prefix: &str) -> Self {
        Self {
            jobs: HashMap::new(),
            prefix: prefix.to_string(),
            next_id: 1,
        }
    }

    /// Submit a new job. Returns the generated job ID.
    ///
    /// The job enters `queued` status and will only be claimable by the
    /// specified `worker`.
    pub fn submit(&mut self, payload: J, worker: &str, queued_by: &str) -> String {
        let id = format!("{}-{:03}", self.prefix, self.next_id);
        self.next_id += 1;

        let job = Job {
            id: id.clone(),
            payload,
            status: JobStatus::Queued,
            worker: worker.to_string(),
            queued_by: queued_by.to_string(),
            queued_at: now_ms(),
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
        };

        self.jobs.insert(id.clone(), job);
        id
    }

    /// Claim the next queued job for a worker.
    ///
    /// Atomically transitions `queued → running`. Returns `None` if no
    /// queued jobs match the worker.
    pub fn claim(&mut self, worker: &str) -> Option<&Job<J>> {
        // Pick the earliest-queued job for this worker (deterministic ordering).
        let id = self
            .jobs
            .values()
            .filter(|j| j.status == JobStatus::Queued && j.worker == worker)
            .min_by_key(|j| j.queued_at)
            .map(|j| j.id.clone())?;

        let job = self.jobs.get_mut(&id)?;
        job.status = JobStatus::Running;
        job.started_at = Some(now_ms());

        self.jobs.get(&id)
    }

    /// Complete a running job with a result.
    ///
    /// Returns `false` if the job doesn't exist or isn't running.
    pub fn complete(&mut self, id: &str, result: serde_json::Value) -> bool {
        if let Some(job) = self.jobs.get_mut(id) {
            if job.status != JobStatus::Running {
                return false;
            }
            job.status = JobStatus::Complete;
            job.completed_at = Some(now_ms());
            job.result = Some(result);
            true
        } else {
            false
        }
    }

    /// Fail a running job with an error message.
    ///
    /// Returns `false` if the job doesn't exist or isn't running.
    pub fn fail(&mut self, id: &str, error: &str) -> bool {
        if let Some(job) = self.jobs.get_mut(id) {
            if job.status != JobStatus::Running {
                return false;
            }
            job.status = JobStatus::Failed;
            job.completed_at = Some(now_ms());
            job.error = Some(error.to_string());
            true
        } else {
            false
        }
    }

    /// List jobs, optionally filtered by status. Sorted by queue time.
    pub fn list(&self, status: Option<&JobStatus>) -> Vec<&Job<J>> {
        let mut jobs: Vec<&Job<J>> = self
            .jobs
            .values()
            .filter(|j| status.is_none_or(|s| &j.status == s))
            .collect();
        jobs.sort_by_key(|j| j.queued_at);
        jobs
    }

    /// Get a job by ID.
    pub fn get(&self, id: &str) -> Option<&Job<J>> {
        self.jobs.get(id)
    }

    /// Get a mutable reference to a job by ID.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Job<J>> {
        self.jobs.get_mut(id)
    }

    /// Number of jobs in the queue.
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Count jobs by status.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let (mut q, mut r, mut c, mut f) = (0, 0, 0, 0);
        for job in self.jobs.values() {
            match job.status {
                JobStatus::Queued => q += 1,
                JobStatus::Running => r += 1,
                JobStatus::Complete => c += 1,
                JobStatus::Failed => f += 1,
            }
        }
        (q, r, c, f)
    }
}

impl<J: Clone> Default for JobQueue<J> {
    fn default() -> Self {
        Self::new("JOB")
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestPayload {
        name: String,
    }

    fn payload(name: &str) -> TestPayload {
        TestPayload {
            name: name.to_string(),
        }
    }

    #[test]
    fn submit_creates_job_with_queued_status() {
        let mut q = JobQueue::new("TEST");
        let id = q.submit(payload("a"), "worker-1", "admin");
        assert_eq!(id, "TEST-001");
        let job = q.get("TEST-001").unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.payload.name, "a");
        assert_eq!(job.worker, "worker-1");
        assert_eq!(job.queued_by, "admin");
    }

    #[test]
    fn ids_increment_sequentially() {
        let mut q = JobQueue::new("BUILD");
        assert_eq!(q.submit(payload("a"), "w", "u"), "BUILD-001");
        assert_eq!(q.submit(payload("b"), "w", "u"), "BUILD-002");
        assert_eq!(q.submit(payload("c"), "w", "u"), "BUILD-003");
    }

    #[test]
    fn claim_transitions_to_running() {
        let mut q = JobQueue::new("T");
        q.submit(payload("a"), "w1", "u");
        let job = q.claim("w1").unwrap();
        assert_eq!(job.status, JobStatus::Running);
        assert!(job.started_at.is_some());
    }

    #[test]
    fn claim_returns_none_when_empty() {
        let mut q = JobQueue::<TestPayload>::new("T");
        assert!(q.claim("w1").is_none());
    }

    #[test]
    fn claim_returns_none_after_all_claimed() {
        let mut q = JobQueue::new("T");
        q.submit(payload("a"), "w1", "u");
        q.claim("w1");
        assert!(q.claim("w1").is_none());
    }

    #[test]
    fn claim_filters_by_worker() {
        let mut q = JobQueue::new("T");
        q.submit(payload("for-w1"), "w1", "u");
        q.submit(payload("for-w2"), "w2", "u");

        let job = q.claim("w2").unwrap();
        assert_eq!(job.payload.name, "for-w2");

        let job = q.claim("w1").unwrap();
        assert_eq!(job.payload.name, "for-w1");
    }

    #[test]
    fn complete_sets_result() {
        let mut q = JobQueue::new("T");
        q.submit(payload("a"), "w", "u");
        q.claim("w");
        assert!(q.complete("T-001", json!({"output": "done"})));

        let job = q.get("T-001").unwrap();
        assert_eq!(job.status, JobStatus::Complete);
        assert!(job.completed_at.is_some());
        assert_eq!(job.result, Some(json!({"output": "done"})));
    }

    #[test]
    fn fail_sets_error() {
        let mut q = JobQueue::new("T");
        q.submit(payload("a"), "w", "u");
        q.claim("w");
        assert!(q.fail("T-001", "OOM"));

        let job = q.get("T-001").unwrap();
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error.as_deref(), Some("OOM"));
    }

    #[test]
    fn cannot_complete_queued_job() {
        let mut q = JobQueue::new("T");
        q.submit(payload("a"), "w", "u");
        assert!(!q.complete("T-001", json!(null)));
    }

    #[test]
    fn cannot_fail_queued_job() {
        let mut q = JobQueue::new("T");
        q.submit(payload("a"), "w", "u");
        assert!(!q.fail("T-001", "err"));
    }

    #[test]
    fn list_filters_by_status() {
        let mut q = JobQueue::new("T");
        q.submit(payload("a"), "w", "u");
        q.submit(payload("b"), "w", "u");
        q.claim("w"); // T-001 → running

        assert_eq!(q.list(Some(&JobStatus::Queued)).len(), 1);
        assert_eq!(q.list(Some(&JobStatus::Running)).len(), 1);
        assert_eq!(q.list(None).len(), 2);
    }

    #[test]
    fn counts_are_correct() {
        let mut q = JobQueue::new("T");
        q.submit(payload("a"), "w", "u");
        q.submit(payload("b"), "w", "u");
        q.submit(payload("c"), "w", "u");

        // Claim + complete T-001
        let j1 = q.claim("w").expect("claim T-001");
        let j1_id = j1.id.clone();
        assert!(q.complete(&j1_id, json!(null)));

        // Claim + fail T-002
        let j2 = q.claim("w").expect("claim T-002");
        let j2_id = j2.id.clone();
        assert!(q.fail(&j2_id, "err"));

        let (queued, running, complete, failed) = q.counts();
        assert_eq!(queued, 1);
        assert_eq!(running, 0);
        assert_eq!(complete, 1);
        assert_eq!(failed, 1);
    }

    #[test]
    fn status_roundtrip() {
        for status in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Complete,
            JobStatus::Failed,
        ] {
            assert_eq!(JobStatus::parse(status.as_str()).unwrap(), status);
        }
        assert!(JobStatus::parse("unknown").is_none());
    }

    #[test]
    fn status_serde_roundtrip() {
        let status = JobStatus::Running;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"running\"");
        let parsed: JobStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, status);
    }
}
