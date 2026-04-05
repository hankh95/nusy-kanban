//! NATS KV-backed training queue — distributed GPU job coordination.
//!
//! EX-3313: Replaces file-based `training_queue.json` with NATS KV for
//! real-time multi-agent GPU training coordination. When `--server` is
//! provided, training commands go through NATS KV instead of local files.
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
    // Format as seconds since epoch — sufficient for ordering and display.
    // Full ISO 8601 would require chrono; epoch seconds are unambiguous.
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

    /// List all jobs, optionally filtered by status.
    pub async fn list_jobs(
        &self,
        status_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let keys = self.kv.keys().await.map_err(|e| e.to_string())?;
        let mut jobs = Vec::new();

        for key in keys {
            if let Some(value) = self.kv.get(&key).await.map_err(|e| e.to_string())? {
                if let Some(filter) = status_filter {
                    if value.get("status").and_then(|s| s.as_str()) != Some(filter) {
                        continue;
                    }
                }
                jobs.push(value);
            }
        }

        Ok(jobs)
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
            "  {:<12} {:<12} {:<25} {:<10} {:<8} {}",
            "Job ID", "Experiment", "Being", "Machine", "Status", "Queued By"
        ));
        lines.push(format!("  {}", "-".repeat(85)));

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
            let queued_by = job.get("queued_by").and_then(|v| v.as_str()).unwrap_or("-");

            lines.push(format!(
                "  {:<12} {:<12} {:<25} {:<10} {:<8} {}",
                id,
                exp,
                truncate(being, 25),
                machine,
                status,
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
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
