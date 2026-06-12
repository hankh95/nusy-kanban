//! NATS KV-backed training queue — distributed GPU job coordination.
//!
//! EX-3313: Replaces file-based `training_queue.json` with NATS KV for
//! real-time multi-agent GPU training coordination. When `--server` is
//! provided, training commands go through NATS KV instead of local files.
//!
//! EX-3447: Extended to carry full TrainingPayload (15-20 fields).
//!
//! Falls back to file-based storage when NATS is unavailable.

use noesis_ship::kv::KvStore;
use noesis_ship::types::{KvBucketConfig, NatsConfig};
use serde_json::json;

/// ISO 8601 timestamp from system clock (no chrono dependency).
fn now_iso8601() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", d.as_secs())
}

/// NATS KV bucket name for training jobs.
const BUCKET_NAME: &str = "training_queue";

/// NATS KV-backed training queue.
///
/// Each job is stored as `job:{TRAIN-XXX}` in the KV bucket. The queue
/// is distributed — any agent can enqueue, any machine can claim.
pub struct NatsTrainingQueue {
    kv: KvStore,
    connected: bool,
}

impl NatsTrainingQueue {
    /// Create a new NATS-backed training queue (not yet connected).
    pub fn new(nats_url: &str) -> Self {
        let config = NatsConfig::new(nats_url);
        let bucket_config = KvBucketConfig {
            bucket: BUCKET_NAME.to_string(),
            history: 5,
            ttl: None,
        };
        Self {
            kv: KvStore::new(bucket_config, config),
            connected: false,
        }
    }

    /// Connect to NATS KV. Returns error if connection fails.
    pub async fn connect(&mut self) -> Result<(), String> {
        self.kv
            .connect()
            .await
            .map_err(|e| format!("training queue NATS KV connect failed: {e}"))?;
        self.connected = true;
        Ok(())
    }

    /// Queue a new training job. Returns the job ID.
    pub async fn queue_job(
        &self,
        experiment_id: &str,
        being: &str,
        corpus: &str,
        machine: &str,
        queued_by: &str,
    ) -> Result<String, String> {
        // Get next ID by counting existing keys
        let keys = self.kv.keys().await.map_err(|e| e.to_string())?;
        let next_num = keys.len() + 1;
        let job_id = format!("TRAIN-{next_num:03}");

        let job = json!({
            "id": job_id,
            "payload": {
                "experiment_id": experiment_id,
                "being": being,
                "corpus": corpus,
            },
            "worker": machine,
            "queued_by": queued_by,
            "status": "queued",
            "queued_at": now_iso8601(),
            "started_at": null,
            "completed_at": null,
            "error": null,
            "result": null,
        });

        self.kv
            .put(&format!("job.{job_id}"), &job)
            .await
            .map_err(|e| e.to_string())?;

        Ok(job_id)
    }

    /// Queue a training job with extended payload. Returns the job ID.
    pub async fn queue_extended(
        &self,
        payload: &crate::training_queue::TrainingPayload,
        machine: &str,
        queued_by: &str,
    ) -> Result<String, String> {
        let keys = self.kv.keys().await.map_err(|e| e.to_string())?;
        let next_num = keys.len() + 1;
        let job_id = format!("TRAIN-{next_num:03}");

        let payload_json =
            serde_json::to_value(payload).map_err(|e| format!("payload serialize failed: {e}"))?;

        let job = json!({
            "id": job_id,
            "payload": payload_json,
            "worker": machine,
            "queued_by": queued_by,
            "status": "queued",
            "queued_at": now_iso8601(),
            "started_at": null,
            "completed_at": null,
            "error": null,
            "result": null,
        });

        self.kv
            .put(&format!("job.{job_id}"), &job)
            .await
            .map_err(|e| e.to_string())?;

        Ok(job_id)
    }

    /// List all jobs, optionally filtered by status.
    pub async fn list_jobs(
        &self,
        status_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let keys = self.kv.keys().await.map_err(|e| e.to_string())?;
        let mut jobs = Vec::new();

        for key in keys {
            if let Some(value) = self.kv.get(&key).await.map_err(|e| e.to_string())? {
                if let Some(filter) = status_filter
                    && value.get("status").and_then(|s| s.as_str()) != Some(filter)
                {
                    continue;
                }
                jobs.push(value);
            }
        }

        Ok(jobs)
    }

    /// Get a specific job by ID.
    pub async fn get_job(&self, job_id: &str) -> Result<Option<serde_json::Value>, String> {
        let key = format!("job.{job_id}");
        self.kv.get(&key).await.map_err(|e| e.to_string())
    }

    /// Claim the next queued job for a machine.
    pub async fn claim_job(&self, machine: &str) -> Result<Option<serde_json::Value>, String> {
        let keys = self.kv.keys().await.map_err(|e| e.to_string())?;

        for key in keys {
            if let Some(mut value) = self.kv.get(&key).await.map_err(|e| e.to_string())? {
                let is_queued = value.get("status").and_then(|s| s.as_str()) == Some("queued");
                let matches_machine = value.get("worker").and_then(|w| w.as_str()) == Some(machine);

                if is_queued && matches_machine {
                    value["status"] = json!("running");
                    value["started_at"] = json!(now_iso8601());
                    self.kv.put(&key, &value).await.map_err(|e| e.to_string())?;
                    return Ok(Some(value));
                }
            }
        }

        Ok(None)
    }

    /// Complete a job with results path.
    pub async fn complete_job(&self, job_id: &str, results_path: &str) -> Result<bool, String> {
        let key = format!("job.{job_id}");
        if let Some(mut value) = self.kv.get(&key).await.map_err(|e| e.to_string())? {
            if value.get("status").and_then(|s| s.as_str()) != Some("running") {
                return Ok(false);
            }
            value["status"] = json!("complete");
            value["completed_at"] = json!(now_iso8601());
            value["result"] = json!({ "results_path": results_path });
            self.kv.put(&key, &value).await.map_err(|e| e.to_string())?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Fail a job with an error message.
    pub async fn fail_job(&self, job_id: &str, error: &str) -> Result<bool, String> {
        let key = format!("job.{job_id}");
        if let Some(mut value) = self.kv.get(&key).await.map_err(|e| e.to_string())? {
            if value.get("status").and_then(|s| s.as_str()) != Some("running") {
                return Ok(false);
            }
            value["status"] = json!("failed");
            value["completed_at"] = json!(now_iso8601());
            value["error"] = json!(error);
            self.kv.put(&key, &value).await.map_err(|e| e.to_string())?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Format jobs for display.
    pub fn format_jobs(jobs: &[serde_json::Value]) -> String {
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
            let id = job.get("id").and_then(|v| v.as_str()).unwrap_or("-");
            let exp = job
                .pointer("/payload/experiment_id")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let being = job
                .pointer("/payload/being")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let machine = job.get("worker").and_then(|v| v.as_str()).unwrap_or("-");
            let status = job.get("status").and_then(|v| v.as_str()).unwrap_or("-");
            let priority = job
                .pointer("/payload/priority")
                .and_then(|v| v.as_str())
                .unwrap_or("normal");
            let queued_by = job.get("queued_by").and_then(|v| v.as_str()).unwrap_or("-");

            lines.push(format!(
                "  {:<12} {:<12} {:<25} {:<10} {:<8} {:<10} {}",
                id,
                exp,
                truncate(being, 25),
                machine,
                status,
                priority,
                queued_by,
            ));
        }

        let queued = jobs
            .iter()
            .filter(|j| j.get("status").and_then(|s| s.as_str()) == Some("queued"))
            .count();
        let running = jobs
            .iter()
            .filter(|j| j.get("status").and_then(|s| s.as_str()) == Some("running"))
            .count();
        let complete = jobs
            .iter()
            .filter(|j| j.get("status").and_then(|s| s.as_str()) == Some("complete"))
            .count();
        let failed = jobs
            .iter()
            .filter(|j| j.get("status").and_then(|s| s.as_str()) == Some("failed"))
            .count();

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
    pub fn format_job_detail(job: &serde_json::Value) -> String {
        let mut lines = Vec::new();

        let id = job.get("id").and_then(|v| v.as_str()).unwrap_or("-");
        let status = job.get("status").and_then(|v| v.as_str()).unwrap_or("-");
        let machine = job.get("worker").and_then(|v| v.as_str()).unwrap_or("-");
        let priority = job
            .pointer("/payload/priority")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");
        let queued_by = job.get("queued_by").and_then(|v| v.as_str()).unwrap_or("-");

        lines.push(format!("  Job: {id}"));
        lines.push(format!(
            "  Status: {status}  Machine: {machine}  Priority: {priority}"
        ));
        lines.push(format!("  Queued by: {queued_by}"));

        if let Some(v) = job.get("queued_at").and_then(|v| v.as_str()) {
            lines.push(format!("  Queued at: {v}"));
        }
        if let Some(v) = job.get("started_at").and_then(|v| v.as_str()) {
            lines.push(format!("  Started at: {v}"));
        }
        if let Some(v) = job.get("completed_at").and_then(|v| v.as_str()) {
            lines.push(format!("  Completed at: {v}"));
        }

        lines.push(String::new());
        lines.push("  Payload:".to_string());

        // Core fields
        if let Some(v) = job
            .pointer("/payload/experiment_id")
            .and_then(|v| v.as_str())
        {
            lines.push(format!("    Experiment: {v}"));
        }
        if let Some(v) = job.pointer("/payload/being").and_then(|v| v.as_str()) {
            lines.push(format!("    Being:      {v}"));
        }
        if let Some(v) = job.pointer("/payload/corpus").and_then(|v| v.as_str()) {
            lines.push(format!("    Corpus:     {v}"));
        }

        // Model configuration
        if let Some(v) = job.pointer("/payload/base_model").and_then(|v| v.as_str()) {
            lines.push(format!("    Base model: {v}"));
        }
        if let Some(v) = job
            .pointer("/payload/adapter_path")
            .and_then(|v| v.as_str())
        {
            lines.push(format!("    Adapter:    {v}"));
        }
        if let Some(v) = job.pointer("/payload/phases").and_then(|v| v.as_u64()) {
            lines.push(format!("    Phases:     {v}"));
        }
        if let Some(v) = job.pointer("/payload/batch_size").and_then(|v| v.as_u64()) {
            lines.push(format!("    Batch size: {v}"));
        }

        // Training parameters
        if let Some(v) = job
            .pointer("/payload/learning_rate")
            .and_then(|v| v.as_f64())
        {
            lines.push(format!("    LR:         {v}"));
        }
        if let Some(v) = job
            .pointer("/payload/cq_threshold")
            .and_then(|v| v.as_f64())
        {
            lines.push(format!("    CQ thresh:  {v}"));
        }
        if let Some(v) = job
            .pointer("/payload/ethical_ratio")
            .and_then(|v| v.as_f64())
        {
            lines.push(format!("    Ethical %:  {v}"));
        }

        // Feature flags
        if let Some(true) = job.pointer("/payload/quantize").and_then(|v| v.as_bool()) {
            lines.push("    QLoRA:      enabled".to_string());
        }
        if let Some(true) = job.pointer("/payload/dual_loss").and_then(|v| v.as_bool()) {
            lines.push("    Dual loss:  enabled".to_string());
        }

        // Data paths
        if let Some(v) = job
            .pointer("/payload/curriculum_path")
            .and_then(|v| v.as_str())
        {
            lines.push(format!("    Curriculum: {v}"));
        }
        if let Some(v) = job
            .pointer("/payload/ethical_qa_path")
            .and_then(|v| v.as_str())
        {
            lines.push(format!("    Ethical QA: {v}"));
        }
        if let Some(v) = job
            .pointer("/payload/checkpoint_dir")
            .and_then(|v| v.as_str())
        {
            lines.push(format!("    Checkpoints:{v}"));
        }

        // Eval spec
        if let Some(v) = job
            .pointer("/payload/run_eval_after")
            .and_then(|v| v.as_bool())
        {
            lines.push(format!("    Eval after: {v}"));
        }
        if let Some(v) = job
            .pointer("/payload/eval_battery_path")
            .and_then(|v| v.as_str())
        {
            lines.push(format!("    Eval path:  {v}"));
        }
        if let Some(v) = job
            .pointer("/payload/eval_battery_tier")
            .and_then(|v| v.as_str())
        {
            lines.push(format!("    Eval tier:  {v}"));
        }

        // COG / cascade
        if let Some(v) = job
            .pointer("/payload/prior_adapter")
            .and_then(|v| v.as_str())
        {
            lines.push(format!("    Prior adap: {v}"));
        }
        if let Some(v) = job
            .pointer("/payload/cascade_levels")
            .and_then(|v| v.as_u64())
        {
            lines.push(format!("    Cascade:    L0→L{v}"));
        }
        if let Some(true) = job
            .pointer("/payload/auto_cog_export")
            .and_then(|v| v.as_bool())
        {
            lines.push("    Auto COG:   enabled".to_string());
        }

        // Hooks
        if let Some(v) = job.pointer("/payload/pre_hook").and_then(|v| v.as_str()) {
            lines.push(format!("    Pre-hook:   {v}"));
        }
        if let Some(v) = job.pointer("/payload/post_hook").and_then(|v| v.as_str()) {
            lines.push(format!("    Post-hook:  {v}"));
        }
        if let Some(v) = job.pointer("/payload/on_failure").and_then(|v| v.as_str()) {
            lines.push(format!("    On-failure: {v}"));
        }

        // Safety
        if let Some(v) = job
            .pointer("/payload/safety_gate")
            .and_then(|v| v.as_bool())
        {
            lines.push(format!("    Safety gate:{v}"));
        }

        // Research linking
        if let Some(v) = job
            .pointer("/payload/hypothesis_id")
            .and_then(|v| v.as_str())
        {
            lines.push(format!("    Hypothesis: {v}"));
        }
        if let Some(v) = job.pointer("/payload/paper_id").and_then(|v| v.as_str()) {
            lines.push(format!("    Paper:      {v}"));
        }

        // Scheduling
        if let Some(v) = job
            .pointer("/payload/estimated_duration_min")
            .and_then(|v| v.as_u64())
        {
            lines.push(format!("    Est. time:  {v} min"));
        }
        if let Some(v) = job.pointer("/payload/depends_on").and_then(|v| v.as_str()) {
            lines.push(format!("    Depends on: {v}"));
        }

        // Experiment isolation — output dir
        let being = job
            .pointer("/payload/being")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let exp = job
            .pointer("/payload/experiment_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let default_dir = format!("research/shared/eval-data/{being}/{exp}");
        let output_dir = job
            .pointer("/payload/output_dir")
            .and_then(|v| v.as_str())
            .unwrap_or(&default_dir);
        lines.push(format!("    Output dir: {output_dir}"));

        // Result / error
        if let Some(result) = job.get("result")
            && !result.is_null()
        {
            lines.push(String::new());
            lines.push(format!("  Result: {result}"));
        }
        if let Some(err) = job.get("error").and_then(|v| v.as_str()) {
            lines.push(String::new());
            lines.push(format!("  Error: {err}"));
        }

        lines.join("\n") + "\n"
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
