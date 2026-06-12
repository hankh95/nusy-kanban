//! Training job queue — coordinate GPU training runs across the fleet.
//!
//! EX-3332: Replaces research/TRAINING-QUEUE.md with a machine-readable queue.
//! CH-3338: Refactored to use noesis-ship's generic `JobQueue<TrainingPayload>`.
//! EX-3447: Extended from 3 fields to production-grade payload (Phases 1-4, 7).
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

/// ACF eval battery tier (EX-3447 Phase 3).
///
/// Each tier targets a different verification surface:
/// - **Smoke** (5 Qs): Model loads, responds, refuses OOD — run every commit
/// - **Incremental** (15 Qs): Targeted to what changed — run every PR
/// - **Standard** (35 Qs): All 6 ACF dimensions — run every training job
/// - **Full** (100+ Qs): Causal, counterfactual, multi-turn, probing — major releases
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvalBatteryTier {
    Smoke,
    Incremental,
    Standard,
    Full,
}

impl EvalBatteryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Incremental => "incremental",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "smoke" => Some(Self::Smoke),
            "incremental" => Some(Self::Incremental),
            "standard" => Some(Self::Standard),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

/// Job priority for scheduling (EX-3447 Phase 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

impl Priority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "normal" => Some(Self::Normal),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// Training-specific job payload (EX-3447 extended).
///
/// Fields are grouped by concern. All new fields are `Option<T>` for backward
/// compatibility — existing queue entries (with only 3 fields) still deserialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingPayload {
    // ── Identity ──────────────────────────────────────────────────────────
    pub experiment_id: String,
    pub being: String,
    pub corpus: String,

    // ── Model configuration ───────────────────────────────────────────────
    /// Base model name (e.g., "Qwen/Qwen2.5-3B-Instruct").
    #[serde(default)]
    pub base_model: Option<String>,
    /// Path to prior adapter for incremental training.
    #[serde(default)]
    pub adapter_path: Option<String>,
    /// Number of training phases (cascade levels).
    #[serde(default)]
    pub phases: Option<u32>,
    /// Per-GPU batch size.
    #[serde(default)]
    pub batch_size: Option<u32>,

    // ── Training parameters ───────────────────────────────────────────────
    /// Learning rate.
    #[serde(default)]
    pub learning_rate: Option<f64>,
    /// CQ (Comprehension Quality) threshold for phase gating.
    #[serde(default)]
    pub cq_threshold: Option<f64>,
    /// Ratio of ethical Q&A to curriculum examples (0.0–1.0).
    #[serde(default)]
    pub ethical_ratio: Option<f64>,

    // ── Feature flags ─────────────────────────────────────────────────────
    /// Use QLoRA quantization (4-bit).
    #[serde(default)]
    pub quantize: Option<bool>,
    /// Use dual-loss training (knowledge + safety heads).
    #[serde(default)]
    pub dual_loss: Option<bool>,

    // ── Data paths ────────────────────────────────────────────────────────
    /// Path to curriculum data.
    #[serde(default)]
    pub curriculum_path: Option<String>,
    /// Path to ethical Q&A pairs for interleaving.
    #[serde(default)]
    pub ethical_qa_path: Option<String>,
    /// Checkpoint output directory.
    #[serde(default)]
    pub checkpoint_dir: Option<String>,

    // ── Eval specification (Phase 3) ──────────────────────────────────────
    /// Run eval battery after training completes.
    #[serde(default)]
    pub run_eval_after: Option<bool>,
    /// Path to eval battery questions.
    #[serde(default)]
    pub eval_battery_path: Option<String>,
    /// Which ACF battery tier to run.
    #[serde(default)]
    pub eval_battery_tier: Option<EvalBatteryTier>,

    // ── COG / cascade ─────────────────────────────────────────────────────
    /// Path to prior adapter for cascade L0→L3.
    #[serde(default)]
    pub prior_adapter: Option<String>,
    /// Number of cascade levels.
    #[serde(default)]
    pub cascade_levels: Option<u32>,
    /// Auto-export COG on training success.
    #[serde(default)]
    pub auto_cog_export: Option<bool>,

    // ── Hooks (Phase 2) ───────────────────────────────────────────────────
    /// Shell command to run before training starts.
    #[serde(default)]
    pub pre_hook: Option<String>,
    /// Shell command to run after training succeeds.
    #[serde(default)]
    pub post_hook: Option<String>,
    /// Shell command to run on training failure.
    #[serde(default)]
    pub on_failure: Option<String>,

    // ── Safety (Phase 7) ──────────────────────────────────────────────────
    /// Require causal safety gate: do(training) → no-effect-on-safety proof.
    #[serde(default)]
    pub safety_gate: Option<bool>,

    // ── Research linking ──────────────────────────────────────────────────
    /// Hypothesis ID being tested (e.g., "H-108").
    #[serde(default)]
    pub hypothesis_id: Option<String>,
    /// Paper ID this data contributes to (e.g., "PAPER-4030").
    #[serde(default)]
    pub paper_id: Option<String>,

    // ── Scheduling ────────────────────────────────────────────────────────
    /// Job priority (low/normal/high/critical).
    #[serde(default)]
    pub priority: Option<Priority>,
    /// Estimated runtime in minutes.
    #[serde(default)]
    pub estimated_duration_min: Option<u32>,
    /// Job IDs this job depends on (comma-separated, e.g., "TRAIN-001,TRAIN-002").
    #[serde(default)]
    pub depends_on: Option<String>,

    // ── Experiment isolation (Phase 4) ────────────────────────────────────
    /// Output directory for this job's artifacts.
    /// Default: `research/shared/eval-data/{being}/{experiment_id}/`
    #[serde(default)]
    pub output_dir: Option<String>,
}

impl TrainingPayload {
    /// Generate default output directory for experiment isolation (Phase 4).
    pub fn default_output_dir(&self) -> String {
        format!(
            "research/shared/eval-data/{}/{}",
            self.being, self.experiment_id
        )
    }

    /// Get the effective output directory (explicit or default).
    pub fn effective_output_dir(&self) -> String {
        self.output_dir
            .clone()
            .unwrap_or_else(|| self.default_output_dir())
    }

    /// Numeric priority for sorting (higher = claimed first).
    pub fn priority_rank(&self) -> u32 {
        match self.priority {
            Some(ref p) => match p {
                Priority::Critical => 4,
                Priority::High => 3,
                Priority::Normal => 2,
                Priority::Low => 1,
            },
            None => 2, // default Normal
        }
    }
}

/// Execute a hook shell command. Returns Ok(()) on success, Err with context on failure.
///
/// Hooks are optional shell commands that run at lifecycle points:
/// - `pre_hook`: before training starts (after claim)
/// - `post_hook`: after training succeeds
/// - `on_failure`: after training fails
///
/// The command runs via `/bin/sh -c` for portability. Environment variables
/// `TRAIN_JOB_ID`, `TRAIN_BEING`, `TRAIN_EXPERIMENT`, `TRAIN_OUTPUT_DIR` are set.
pub fn execute_hook(
    hook_name: &str,
    command: &str,
    job_id: &str,
    payload: &TrainingPayload,
) -> Result<(), String> {
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .env("TRAIN_JOB_ID", job_id)
        .env("TRAIN_BEING", &payload.being)
        .env("TRAIN_EXPERIMENT", &payload.experiment_id)
        .env("TRAIN_OUTPUT_DIR", payload.effective_output_dir())
        .output()
        .map_err(|e| format!("{hook_name} hook failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{hook_name} hook exited with code {:?}: {stderr}",
            output.status.code()
        ));
    }

    eprintln!("  {hook_name} hook completed for {job_id}");
    Ok(())
}

/// Run pre_hook if present in the payload.
pub fn run_pre_hook(job_id: &str, payload: &TrainingPayload) -> Result<(), String> {
    if let Some(ref cmd) = payload.pre_hook {
        execute_hook("pre", cmd, job_id, payload)?;
    }
    Ok(())
}

/// Run post_hook if present in the payload.
pub fn run_post_hook(job_id: &str, payload: &TrainingPayload) -> Result<(), String> {
    if let Some(ref cmd) = payload.post_hook {
        execute_hook("post", cmd, job_id, payload)?;
    }
    Ok(())
}

/// Run on_failure hook if present in the payload.
pub fn run_failure_hook(job_id: &str, payload: &TrainingPayload) -> Result<(), String> {
    if let Some(ref cmd) = payload.on_failure {
        // Best-effort: log error but don't propagate failure hook errors
        if let Err(e) = execute_hook("on_failure", cmd, job_id, payload) {
            eprintln!("  WARNING: {e}");
        }
    }
    Ok(())
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

    /// Queue a new training job with extended payload. Returns the job ID.
    pub fn queue_job(
        &mut self,
        experiment_id: &str,
        being: &str,
        corpus: &str,
        machine: &str,
        queued_by: &str,
    ) -> String {
        self.queue_extended(
            TrainingPayload {
                experiment_id: experiment_id.to_string(),
                being: being.to_string(),
                corpus: corpus.to_string(),
                base_model: None,
                adapter_path: None,
                phases: None,
                batch_size: None,
                learning_rate: None,
                cq_threshold: None,
                ethical_ratio: None,
                quantize: None,
                dual_loss: None,
                curriculum_path: None,
                ethical_qa_path: None,
                checkpoint_dir: None,
                run_eval_after: None,
                eval_battery_path: None,
                eval_battery_tier: None,
                prior_adapter: None,
                cascade_levels: None,
                auto_cog_export: None,
                pre_hook: None,
                post_hook: None,
                on_failure: None,
                safety_gate: None,
                hypothesis_id: None,
                paper_id: None,
                priority: None,
                estimated_duration_min: None,
                depends_on: None,
                output_dir: None,
            },
            machine,
            queued_by,
        )
    }

    /// Queue a new training job with a full payload. Returns the job ID.
    pub fn queue_extended(
        &mut self,
        payload: TrainingPayload,
        machine: &str,
        queued_by: &str,
    ) -> String {
        self.inner.submit(payload, machine, queued_by)
    }

    /// Claim the next queued job for a machine, respecting priority and dependencies.
    ///
    /// Jobs are sorted by priority (critical first). A job is only claimed if all
    /// its `depends_on` jobs are complete.
    pub fn claim_job(&mut self, machine: &str) -> Option<&TrainingJob> {
        // Collect all queued job IDs for this machine, sorted by priority desc
        let candidates: Vec<String> = {
            let mut queued: Vec<_> = self
                .inner
                .list(Some(&JobStatus::Queued))
                .into_iter()
                .filter(|j| j.worker == machine)
                .collect();

            // Sort by priority rank descending (critical first)
            queued.sort_by(|a, b| b.payload.priority_rank().cmp(&a.payload.priority_rank()));

            queued.iter().map(|j| j.id.clone()).collect()
        };

        // Find first candidate whose dependencies are all complete
        for job_id in &candidates {
            // Check dependency — need to get the payload
            let met = self
                .inner
                .get(job_id)
                .is_some_and(|j| self.dependencies_met(&j.payload));
            if met {
                return self.claim_by_id(job_id);
            }
        }

        None
    }

    /// Claim a specific job by ID, transitioning queued → running.
    fn claim_by_id(&mut self, job_id: &str) -> Option<&TrainingJob> {
        use noesis_ship::job_queue::JobStatus;
        let job = self.inner.get_mut(job_id)?;
        if job.status != JobStatus::Queued {
            return None;
        }
        job.status = JobStatus::Running;
        job.started_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        );
        self.inner.get(job_id)
    }

    /// Check whether all depends_on jobs for a payload are complete.
    fn dependencies_met(&self, payload: &TrainingPayload) -> bool {
        let Some(ref deps) = payload.depends_on else {
            return true; // no dependencies
        };

        for dep_id in deps.split(',') {
            let dep_id = dep_id.trim();
            if dep_id.is_empty() {
                continue;
            }
            match self.inner.get(dep_id) {
                Some(job) => {
                    if job.status != JobStatus::Complete {
                        return false; // dependency not yet complete
                    }
                }
                None => return false, // dependency not found
            }
        }
        true
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
            "  {:<12} {:<12} {:<25} {:<10} {:<8} {:<10} {}",
            "Job ID", "Experiment", "Being", "Machine", "Status", "Priority", "Queued By"
        ));
        lines.push(format!("  {}", "-".repeat(95)));

        for job in jobs {
            let priority = job
                .payload
                .priority
                .as_ref()
                .map(|p| p.as_str())
                .unwrap_or("normal");
            lines.push(format!(
                "  {:<12} {:<12} {:<25} {:<10} {:<8} {:<10} {}",
                job.id,
                job.payload.experiment_id,
                truncate(&job.payload.being, 25),
                job.worker,
                job.status.as_str(),
                priority,
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

    /// Format a single job's full details for display.
    pub fn format_job_detail(job: &TrainingJob) -> String {
        let mut lines = Vec::new();
        lines.push(format!("  Job: {}", job.id));
        lines.push(format!(
            "  Status: {}  Machine: {}  Priority: {}",
            job.status.as_str(),
            job.worker,
            job.payload
                .priority
                .as_ref()
                .map(|p| p.as_str())
                .unwrap_or("normal")
        ));
        lines.push(format!(
            "  Queued by: {} at {}",
            job.queued_by, job.queued_at
        ));
        if let Some(t) = job.started_at {
            lines.push(format!("  Started at: {t}"));
        }
        if let Some(t) = job.completed_at {
            lines.push(format!("  Completed at: {t}"));
        }
        lines.push(String::new());
        lines.push("  Payload:".to_string());
        lines.push(format!("    Experiment: {}", job.payload.experiment_id));
        lines.push(format!("    Being:      {}", job.payload.being));
        lines.push(format!("    Corpus:     {}", job.payload.corpus));

        // Model configuration
        if let Some(ref v) = job.payload.base_model {
            lines.push(format!("    Base model: {v}"));
        }
        if let Some(ref v) = job.payload.adapter_path {
            lines.push(format!("    Adapter:    {v}"));
        }
        if let Some(v) = job.payload.phases {
            lines.push(format!("    Phases:     {v}"));
        }
        if let Some(v) = job.payload.batch_size {
            lines.push(format!("    Batch size: {v}"));
        }

        // Training parameters
        if let Some(v) = job.payload.learning_rate {
            lines.push(format!("    LR:         {v}"));
        }
        if let Some(v) = job.payload.cq_threshold {
            lines.push(format!("    CQ thresh:  {v}"));
        }
        if let Some(v) = job.payload.ethical_ratio {
            lines.push(format!("    Ethical %:  {v}"));
        }

        // Feature flags
        if Some(true) == job.payload.quantize {
            lines.push("    QLoRA:      enabled".to_string());
        }
        if Some(true) == job.payload.dual_loss {
            lines.push("    Dual loss:  enabled".to_string());
        }

        // Data paths
        if let Some(ref v) = job.payload.curriculum_path {
            lines.push(format!("    Curriculum: {v}"));
        }
        if let Some(ref v) = job.payload.ethical_qa_path {
            lines.push(format!("    Ethical QA: {v}"));
        }
        if let Some(ref v) = job.payload.checkpoint_dir {
            lines.push(format!("    Checkpoints:{v}"));
        }

        // Eval spec
        if let Some(v) = job.payload.run_eval_after {
            lines.push(format!("    Eval after: {v}"));
        }
        if let Some(ref v) = job.payload.eval_battery_path {
            lines.push(format!("    Eval path:  {v}"));
        }
        if let Some(ref v) = job.payload.eval_battery_tier {
            lines.push(format!("    Eval tier:  {}", v.as_str()));
        }

        // COG / cascade
        if let Some(ref v) = job.payload.prior_adapter {
            lines.push(format!("    Prior adap: {v}"));
        }
        if let Some(v) = job.payload.cascade_levels {
            lines.push(format!("    Cascade:    L0→L{v}"));
        }
        if Some(true) == job.payload.auto_cog_export {
            lines.push("    Auto COG:   enabled".to_string());
        }

        // Hooks
        if let Some(ref v) = job.payload.pre_hook {
            lines.push(format!("    Pre-hook:   {v}"));
        }
        if let Some(ref v) = job.payload.post_hook {
            lines.push(format!("    Post-hook:  {v}"));
        }
        if let Some(ref v) = job.payload.on_failure {
            lines.push(format!("    On-failure: {v}"));
        }

        // Safety
        if let Some(v) = job.payload.safety_gate {
            lines.push(format!("    Safety gate:{v}"));
        }

        // Research linking
        if let Some(ref v) = job.payload.hypothesis_id {
            lines.push(format!("    Hypothesis: {v}"));
        }
        if let Some(ref v) = job.payload.paper_id {
            lines.push(format!("    Paper:      {v}"));
        }

        // Scheduling
        if let Some(v) = job.payload.estimated_duration_min {
            lines.push(format!("    Est. time:  {v} min"));
        }
        if let Some(ref v) = job.payload.depends_on {
            lines.push(format!("    Depends on: {v}"));
        }

        // Experiment isolation
        lines.push(format!(
            "    Output dir: {}",
            job.payload.effective_output_dir()
        ));

        // Result / error
        if let Some(ref result) = job.result {
            lines.push(String::new());
            lines.push(format!("  Result: {result}"));
        }
        if let Some(ref err) = job.error {
            lines.push(String::new());
            lines.push(format!("  Error: {err}"));
        }

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

    fn full_payload() -> TrainingPayload {
        TrainingPayload {
            experiment_id: "EXPR-3275".to_string(),
            being: "santiago-bahai".to_string(),
            corpus: "bahai".to_string(),
            base_model: Some("Qwen/Qwen2.5-3B-Instruct".to_string()),
            adapter_path: None,
            phases: Some(3),
            batch_size: Some(4),
            learning_rate: Some(2e-4),
            cq_threshold: Some(0.7),
            ethical_ratio: Some(0.15),
            quantize: Some(true),
            dual_loss: None,
            curriculum_path: Some("corpus/bahai/".to_string()),
            ethical_qa_path: None,
            checkpoint_dir: None,
            run_eval_after: Some(true),
            eval_battery_path: None,
            eval_battery_tier: Some(EvalBatteryTier::Standard),
            prior_adapter: None,
            cascade_levels: None,
            auto_cog_export: Some(true),
            pre_hook: None,
            post_hook: Some("nk training eval TRAIN-001".to_string()),
            on_failure: None,
            safety_gate: Some(true),
            hypothesis_id: None,
            paper_id: Some("PAPER-4030".to_string()),
            priority: Some(Priority::High),
            estimated_duration_min: Some(120),
            depends_on: None,
            output_dir: None,
        }
    }

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
        // New fields default to None
        assert_eq!(job.payload.base_model, None);
        assert_eq!(job.payload.priority, None);
    }

    #[test]
    fn test_queue_extended_job() {
        let mut q = TrainingQueue::new();
        let payload = full_payload();
        let id = q.queue_extended(payload, "DGX", "Captain");
        assert_eq!(id, "TRAIN-001");

        let job = q.get_job("TRAIN-001").expect("job");
        assert_eq!(
            job.payload.base_model.as_deref(),
            Some("Qwen/Qwen2.5-3B-Instruct")
        );
        assert_eq!(job.payload.quantize, Some(true));
        assert_eq!(
            job.payload.eval_battery_tier,
            Some(EvalBatteryTier::Standard)
        );
        assert_eq!(job.payload.priority, Some(Priority::High));
        assert_eq!(job.payload.safety_gate, Some(true));
        assert_eq!(job.payload.estimated_duration_min, Some(120));
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
        assert!(output.contains("Priority"));
    }

    #[test]
    fn test_format_table_with_priority() {
        let mut q = TrainingQueue::new();
        let mut payload = full_payload();
        payload.priority = Some(Priority::Critical);
        q.queue_extended(payload, "DGX", "Captain");
        let output = q.format_table();
        assert!(output.contains("critical"));
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

    #[test]
    fn test_eval_battery_tier_roundtrip() {
        for tier in &[
            EvalBatteryTier::Smoke,
            EvalBatteryTier::Incremental,
            EvalBatteryTier::Standard,
            EvalBatteryTier::Full,
        ] {
            let s = tier.as_str();
            assert_eq!(&EvalBatteryTier::parse(s).expect("parse"), tier);
        }
        assert!(EvalBatteryTier::parse("unknown").is_none());
    }

    #[test]
    fn test_priority_roundtrip() {
        for p in &[
            Priority::Low,
            Priority::Normal,
            Priority::High,
            Priority::Critical,
        ] {
            let s = p.as_str();
            assert_eq!(&Priority::parse(s).expect("parse"), p);
        }
        assert!(Priority::parse("unknown").is_none());
    }

    #[test]
    fn test_default_output_dir() {
        let payload = TrainingPayload {
            experiment_id: "EXPR-3275".to_string(),
            being: "santiago-bahai".to_string(),
            corpus: "bahai".to_string(),
            base_model: None,
            adapter_path: None,
            phases: None,
            batch_size: None,
            learning_rate: None,
            cq_threshold: None,
            ethical_ratio: None,
            quantize: None,
            dual_loss: None,
            curriculum_path: None,
            ethical_qa_path: None,
            checkpoint_dir: None,
            run_eval_after: None,
            eval_battery_path: None,
            eval_battery_tier: None,
            prior_adapter: None,
            cascade_levels: None,
            auto_cog_export: None,
            pre_hook: None,
            post_hook: None,
            on_failure: None,
            safety_gate: None,
            hypothesis_id: None,
            paper_id: None,
            priority: None,
            estimated_duration_min: None,
            depends_on: None,
            output_dir: None,
        };
        assert_eq!(
            payload.default_output_dir(),
            "research/shared/eval-data/santiago-bahai/EXPR-3275"
        );
        assert_eq!(
            payload.effective_output_dir(),
            "research/shared/eval-data/santiago-bahai/EXPR-3275"
        );
    }

    #[test]
    fn test_explicit_output_dir() {
        let payload = TrainingPayload {
            output_dir: Some("custom/output/path".to_string()),
            ..full_payload()
        };
        assert_eq!(payload.effective_output_dir(), "custom/output/path");
    }

    #[test]
    fn test_format_job_detail() {
        let mut q = TrainingQueue::new();
        q.queue_extended(full_payload(), "DGX", "Captain");
        let job = q.get_job("TRAIN-001").unwrap();
        let detail = TrainingQueue::format_job_detail(job);
        assert!(detail.contains("TRAIN-001"));
        assert!(detail.contains("EXPR-3275"));
        assert!(detail.contains("santiago-bahai"));
        assert!(detail.contains("Qwen/Qwen2.5-3B-Instruct"));
        assert!(detail.contains("QLoRA:      enabled"));
        assert!(detail.contains("Eval tier:  standard"));
        assert!(detail.contains("high"));
        assert!(detail.contains("Safety gate:true"));
        assert!(detail.contains("120 min"));
        assert!(detail.contains("PAPER-4030"));
    }

    #[test]
    fn test_backward_compat_deserialize() {
        // Old-format JSON with only 3 fields should still deserialize.
        let old_json = r#"{
            "experiment_id": "EXPR-OLD",
            "being": "old-being",
            "corpus": "old-corpus"
        }"#;
        let payload: TrainingPayload =
            serde_json::from_str(old_json).expect("deserialize old format");
        assert_eq!(payload.experiment_id, "EXPR-OLD");
        assert_eq!(payload.base_model, None);
        assert_eq!(payload.priority, None);
        assert_eq!(payload.safety_gate, None);
    }

    #[test]
    fn test_payload_serde_roundtrip() {
        let payload = full_payload();
        let json = serde_json::to_string(&payload).expect("serialize");
        let restored: TrainingPayload = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.experiment_id, payload.experiment_id);
        assert_eq!(restored.base_model, payload.base_model);
        assert_eq!(restored.quantize, payload.quantize);
        assert_eq!(restored.eval_battery_tier, payload.eval_battery_tier);
        assert_eq!(restored.priority, payload.priority);
        assert_eq!(restored.safety_gate, payload.safety_gate);
    }

    // ── Priority-aware claiming tests (EX-4060) ──────────────────────────

    #[test]
    fn test_priority_rank_ordering() {
        let p = TrainingPayload {
            priority: Some(Priority::Critical),
            ..minimal_payload()
        };
        assert_eq!(p.priority_rank(), 4);

        let p = TrainingPayload {
            priority: Some(Priority::High),
            ..minimal_payload()
        };
        assert_eq!(p.priority_rank(), 3);

        let p = TrainingPayload {
            priority: Some(Priority::Normal),
            ..minimal_payload()
        };
        assert_eq!(p.priority_rank(), 2);

        let p = TrainingPayload {
            priority: Some(Priority::Low),
            ..minimal_payload()
        };
        assert_eq!(p.priority_rank(), 1);

        // Default (no priority) is Normal
        let p = TrainingPayload {
            priority: None,
            ..minimal_payload()
        };
        assert_eq!(p.priority_rank(), 2);
    }

    #[test]
    fn test_claim_prefers_higher_priority() {
        let mut q = TrainingQueue::new();

        // Queue low-priority first, then critical
        let mut low = minimal_payload();
        low.experiment_id = "EXPR-LOW".to_string();
        low.priority = Some(Priority::Low);
        q.queue_extended(low, "DGX", "Captain");

        let mut critical = minimal_payload();
        critical.experiment_id = "EXPR-CRITICAL".to_string();
        critical.priority = Some(Priority::Critical);
        q.queue_extended(critical, "DGX", "Captain");

        // Critical should be claimed first despite being queued second
        let job = q.claim_job("DGX").expect("claim");
        assert_eq!(job.payload.experiment_id, "EXPR-CRITICAL");
    }

    #[test]
    fn test_claim_priority_across_machines() {
        let mut q = TrainingQueue::new();

        let mut high = minimal_payload();
        high.experiment_id = "EXPR-HIGH".to_string();
        high.priority = Some(Priority::High);
        q.queue_extended(high, "DGX", "Captain");

        let mut low = minimal_payload();
        low.experiment_id = "EXPR-LOW".to_string();
        low.priority = Some(Priority::Low);
        q.queue_extended(low, "Mini", "Captain");

        // Each machine should claim its own highest-priority job
        let dgx_job = q.claim_job("DGX").expect("DGX claim");
        assert_eq!(dgx_job.payload.experiment_id, "EXPR-HIGH");

        let mini_job = q.claim_job("Mini").expect("Mini claim");
        assert_eq!(mini_job.payload.experiment_id, "EXPR-LOW");
    }

    // ── Dependency checking tests (EX-4060) ──────────────────────────────

    #[test]
    fn test_dependencies_met_no_deps() {
        let q = TrainingQueue::new();
        let payload = minimal_payload();
        assert!(q.dependencies_met(&payload));
    }

    #[test]
    fn test_dependencies_met_empty_deps() {
        let q = TrainingQueue::new();
        let mut payload = minimal_payload();
        payload.depends_on = Some("".to_string());
        assert!(q.dependencies_met(&payload));
    }

    #[test]
    fn test_dependencies_blocked_by_incomplete() {
        let mut q = TrainingQueue::new();
        // Queue a job but don't complete it
        q.queue_job("EXPR-1", "being", "corpus", "DGX", "Captain");

        let mut payload = minimal_payload();
        payload.depends_on = Some("TRAIN-001".to_string());
        // TRAIN-001 is queued, not complete → blocked
        assert!(!q.dependencies_met(&payload));
    }

    #[test]
    fn test_dependencies_met_after_completion() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-1", "being", "corpus", "DGX", "Captain");
        q.claim_job("DGX");
        q.complete_job("TRAIN-001", "results/");

        let mut payload = minimal_payload();
        payload.depends_on = Some("TRAIN-001".to_string());
        // TRAIN-001 is complete → unblocked
        assert!(q.dependencies_met(&payload));
    }

    #[test]
    fn test_dependencies_met_unknown_job() {
        let q = TrainingQueue::new();
        let mut payload = minimal_payload();
        payload.depends_on = Some("TRAIN-999".to_string());
        // TRAIN-999 doesn't exist → blocked
        assert!(!q.dependencies_met(&payload));
    }

    #[test]
    fn test_dependencies_multiple_all_complete() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-1", "b", "c", "DGX", "C");
        q.queue_job("EXPR-2", "b", "c", "DGX", "C");
        q.claim_job("DGX"); // TRAIN-001
        q.claim_job("DGX"); // TRAIN-002
        q.complete_job("TRAIN-001", "r1/");
        q.complete_job("TRAIN-002", "r2/");

        let mut payload = minimal_payload();
        payload.depends_on = Some("TRAIN-001,TRAIN-002".to_string());
        assert!(q.dependencies_met(&payload));
    }

    #[test]
    fn test_dependencies_multiple_one_incomplete() {
        let mut q = TrainingQueue::new();
        q.queue_job("EXPR-1", "b", "c", "DGX", "C");
        q.queue_job("EXPR-2", "b", "c", "DGX", "C");
        q.claim_job("DGX"); // TRAIN-001
        q.claim_job("DGX"); // TRAIN-002
        q.complete_job("TRAIN-001", "r1/");
        // TRAIN-002 still running

        let mut payload = minimal_payload();
        payload.depends_on = Some("TRAIN-001, TRAIN-002".to_string());
        assert!(!q.dependencies_met(&payload));
    }

    #[test]
    fn test_claim_skips_job_with_unmet_deps() {
        let mut q = TrainingQueue::new();

        // Queue a critical job with unmet dependency
        let mut blocked = minimal_payload();
        blocked.experiment_id = "EXPR-BLOCKED".to_string();
        blocked.priority = Some(Priority::Critical);
        blocked.depends_on = Some("TRAIN-999".to_string()); // doesn't exist
        q.queue_extended(blocked, "DGX", "Captain");

        // Queue a low-priority job with no dependencies
        let mut ready = minimal_payload();
        ready.experiment_id = "EXPR-READY".to_string();
        ready.priority = Some(Priority::Low);
        q.queue_extended(ready, "DGX", "Captain");

        // Should claim the ready job, skipping the blocked critical one
        let job = q.claim_job("DGX").expect("claim");
        assert_eq!(job.payload.experiment_id, "EXPR-READY");
    }

    // ── Hook execution tests (EX-4060) ───────────────────────────────────

    #[test]
    fn test_execute_hook_success() {
        let payload = minimal_payload();
        let result = execute_hook("test", "echo hello", "TRAIN-001", &payload);
        assert!(result.is_ok());
    }

    #[test]
    fn test_execute_hook_failure() {
        let payload = minimal_payload();
        let result = execute_hook("test", "exit 1", "TRAIN-001", &payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("exited with code"));
    }

    #[test]
    fn test_execute_hook_sets_env_vars() {
        let mut payload = minimal_payload();
        payload.experiment_id = "EXPR-TEST".to_string();
        payload.being = "santiago-test".to_string();

        // Hook that verifies env vars by writing to a temp file
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        let cmd = format!("env > {}", out.display());

        execute_hook("test", &cmd, "TRAIN-001", &payload).unwrap();

        let env_content = std::fs::read_to_string(&out).unwrap();
        assert!(env_content.contains("TRAIN_JOB_ID=TRAIN-001"));
        assert!(env_content.contains("TRAIN_BEING=santiago-test"));
        assert!(env_content.contains("TRAIN_EXPERIMENT=EXPR-TEST"));
        assert!(env_content.contains("TRAIN_OUTPUT_DIR="));
    }

    #[test]
    fn test_run_pre_hook_no_hook() {
        let payload = TrainingPayload {
            pre_hook: None,
            ..minimal_payload()
        };
        assert!(run_pre_hook("TRAIN-001", &payload).is_ok());
    }

    #[test]
    fn test_run_post_hook_no_hook() {
        let payload = TrainingPayload {
            post_hook: None,
            ..minimal_payload()
        };
        assert!(run_post_hook("TRAIN-001", &payload).is_ok());
    }

    #[test]
    fn test_run_failure_hook_no_hook() {
        let payload = TrainingPayload {
            on_failure: None,
            ..minimal_payload()
        };
        assert!(run_failure_hook("TRAIN-001", &payload).is_ok());
    }

    #[test]
    fn test_run_failure_hook_best_effort() {
        // on_failure hook errors are swallowed (best-effort)
        let payload = TrainingPayload {
            on_failure: Some("exit 1".to_string()),
            ..minimal_payload()
        };
        assert!(run_failure_hook("TRAIN-001", &payload).is_ok());
    }

    // ── Helper ───────────────────────────────────────────────────────────

    fn minimal_payload() -> TrainingPayload {
        TrainingPayload {
            experiment_id: "EXPR-TEST".to_string(),
            being: "santiago-test".to_string(),
            corpus: "test".to_string(),
            base_model: None,
            adapter_path: None,
            phases: None,
            batch_size: None,
            learning_rate: None,
            cq_threshold: None,
            ethical_ratio: None,
            quantize: None,
            dual_loss: None,
            curriculum_path: None,
            ethical_qa_path: None,
            checkpoint_dir: None,
            run_eval_after: None,
            eval_battery_path: None,
            eval_battery_tier: None,
            prior_adapter: None,
            cascade_levels: None,
            auto_cog_export: None,
            pre_hook: None,
            post_hook: None,
            on_failure: None,
            safety_gate: None,
            hypothesis_id: None,
            paper_id: None,
            priority: None,
            estimated_duration_min: None,
            depends_on: None,
            output_dir: None,
        }
    }
}
