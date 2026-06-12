//! Training runner — bridges queue payloads to torchtune GPU execution.
//!
//! EX-4060: Completes Training Queue 2.0 by wiring claimed jobs to actual
//! GPU training via torchtune subprocess. Converts `TrainingPayload` into
//! torchtune YAML config, runs training, converts adapter to Candle format,
//! and handles pre/post hooks.
//!
//! # Pipeline
//!
//! ```text
//! Claimed job (TrainingPayload)
//!   → execute pre_hook
//!   → convert payload → torchtune YAML config
//!   → run torchtune lora_finetune_single_device
//!   → convert adapter BF16→F32 (Candle compatibility)
//!   → execute post_hook (on success) or on_failure (on error)
//!   → mark job complete/failed
//! ```

use crate::training_queue::{TrainingPayload, run_failure_hook, run_post_hook, run_pre_hook};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Result of a training run.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RunResult {
    /// Path to the Candle-format adapter.
    pub adapter_path: PathBuf,
    /// Training duration in seconds.
    pub duration_secs: f64,
}

/// Training configuration derived from a queue payload.
pub struct RunConfig {
    /// Path to the model weights directory.
    pub model_dir: PathBuf,
    /// Path to the training data (Alpaca JSONL).
    pub data_path: PathBuf,
    /// Output directory for the trained adapter.
    pub output_dir: PathBuf,
    /// LoRA rank.
    pub lora_rank: usize,
    /// LoRA alpha.
    pub lora_alpha: usize,
    /// Number of training epochs.
    pub epochs: usize,
    /// Per-device batch size.
    pub batch_size: usize,
    /// Learning rate.
    pub learning_rate: f64,
    /// Max sequence length.
    pub max_seq_len: usize,
    /// Path to the torchtune venv.
    pub venv_path: PathBuf,
    /// Use QLoRA 4-bit quantization.
    pub quantize: bool,
}

impl RunConfig {
    /// Derive a RunConfig from a TrainingPayload with defaults.
    ///
    /// **Note:** The torchtune config template only supports Qwen2.5-3B-Instruct.
    /// The model component, tokenizer, and checkpoint shard list are all
    /// Qwen2.5-3B-specific. If `base_model` specifies a different model, a
    /// warning is printed and the config may fail at torchtune runtime.
    pub fn from_payload(payload: &TrainingPayload) -> Self {
        let model_name = payload
            .base_model
            .as_deref()
            .unwrap_or("Qwen/Qwen2.5-3B-Instruct");

        // Validate: template only supports Qwen2.5-3B-Instruct
        if !model_name.contains("Qwen2.5-3B") {
            eprintln!(
                "WARNING: RunConfig only supports Qwen2.5-3B-Instruct, got '{model_name}'. \
                 The generated torchtune config will reference Qwen2.5-3B components and \
                 checkpoint shards — training will likely fail."
            );
        }

        let output_dir = PathBuf::from(payload.effective_output_dir());

        Self {
            model_dir: PathBuf::from(model_name),
            data_path: PathBuf::from(
                payload
                    .curriculum_path
                    .as_deref()
                    .unwrap_or("corpus/alpaca.jsonl"),
            ),
            output_dir: output_dir.clone(),
            lora_rank: 16,
            lora_alpha: 32,
            epochs: payload.phases.unwrap_or(3) as usize,
            batch_size: payload.batch_size.unwrap_or(1) as usize,
            learning_rate: payload.learning_rate.unwrap_or(2e-4),
            max_seq_len: 256,
            venv_path: PathBuf::from(".venv-training"),
            quantize: payload.quantize.unwrap_or(false),
        }
    }
}

/// Generate a torchtune YAML config file from a RunConfig.
pub fn generate_torchtune_config(config: &RunConfig, output_path: &Path) -> Result<(), String> {
    let quantizer_section = if config.quantize {
        r#"quantizer:
  _component_: torchtune.training.quantization.Int4WeightOnlyQuantizer
  groupsize: 128"#
    } else {
        ""
    };

    let yaml = format!(
        r#"output_dir: {output_dir}
model:
  _component_: torchtune.models.qwen2_5.lora_qwen2_5_3b
  lora_attn_modules:
  - q_proj
  - v_proj
  apply_lora_to_mlp: false
  lora_rank: {rank}
  lora_alpha: {alpha}
  lora_dropout: 0.05
tokenizer:
  _component_: torchtune.models.qwen2_5.qwen2_5_tokenizer
  path: {model_dir}/vocab.json
  merges_file: {model_dir}/merges.txt
  max_seq_len: {max_seq_len}
checkpointer:
  _component_: torchtune.training.FullModelHFCheckpointer
  checkpoint_dir: {model_dir}
  # NOTE: These shard filenames are specific to Qwen2.5-3B-Instruct (2 shards).
  # Other models will have different shard counts/names — the config must be
  # updated to match the model directory contents.
  checkpoint_files:
  - model-00001-of-00002.safetensors
  - model-00002-of-00002.safetensors
  recipe_checkpoint: null
  output_dir: ${{{{output_dir}}}}
  model_type: QWEN2
resume_from_checkpoint: false
dataset:
  _component_: torchtune.datasets.alpaca_dataset
  source: json
  data_files: {data_path}
  train_on_input: false
  packed: false
seed: 42
shuffle: true
batch_size: {batch_size}
optimizer:
  _component_: torch.optim.AdamW
  fused: true
  weight_decay: 0.01
  lr: {lr}
lr_scheduler:
  _component_: torchtune.training.lr_schedulers.get_cosine_schedule_with_warmup
  num_warmup_steps: 10
loss:
  _component_: torchtune.modules.loss.CEWithChunkedOutputLoss
epochs: {epochs}
max_steps_per_epoch: null
gradient_accumulation_steps: 4
clip_grad_norm: 1.0
compile: false
metric_logger:
  _component_: torchtune.training.metric_logging.DiskLogger
  log_dir: ${{{{output_dir}}}}/logs
log_every_n_steps: 10
log_peak_memory_stats: true
device: cuda
dtype: bf16
enable_activation_checkpointing: true
enable_activation_offloading: false
{quantizer_section}
profiler:
  _component_: torchtune.training.setup_torch_profiler
  enabled: false
  output_dir: ${{{{output_dir}}}}/profiling_outputs
"#,
        output_dir = config.output_dir.display(),
        model_dir = config.model_dir.display(),
        data_path = config.data_path.display(),
        rank = config.lora_rank,
        alpha = config.lora_alpha,
        max_seq_len = config.max_seq_len,
        batch_size = config.batch_size,
        lr = config.learning_rate,
        epochs = config.epochs,
    );

    std::fs::create_dir_all(output_path.parent().unwrap_or(Path::new(".")))
        .map_err(|e| format!("mkdir: {e}"))?;
    std::fs::write(output_path, yaml).map_err(|e| format!("write config: {e}"))?;
    Ok(())
}

/// Run torchtune training as a subprocess.
pub fn run_torchtune(config_path: &Path, venv_path: &Path) -> Result<(), String> {
    let tune_bin = venv_path.join("bin/tune");
    if !tune_bin.exists() {
        return Err(format!(
            "torchtune not found at {:?}. Install: pip install torchtune",
            tune_bin
        ));
    }

    eprintln!("  Running torchtune: {:?}", tune_bin);
    eprintln!("  Config: {:?}", config_path);

    let status = Command::new(&tune_bin)
        .arg("run")
        .arg("lora_finetune_single_device")
        .arg("--config")
        .arg(config_path)
        .status()
        .map_err(|e| format!("torchtune spawn: {e}"))?;

    if !status.success() {
        return Err(format!(
            "torchtune failed with exit code: {:?}",
            status.code()
        ));
    }

    Ok(())
}

/// Convert a torchtune adapter to Candle format.
///
/// 1. Rename keys from torchtune format to Candle format
/// 2. Convert dtype: BF16 → F32 (Candle VarMap requires F32)
pub fn convert_adapter_to_candle(
    torchtune_adapter: &Path,
    candle_output: &Path,
) -> Result<usize, String> {
    let script = r#"
import re, sys
from safetensors.torch import load_file, save_file

src = load_file(sys.argv[1])
converted = {}
skipped = []
for key, tensor in src.items():
    m = re.match(r'base_model\.model\.model\.layers\.(\d+)\.self_attn\.(\w+)\.lora_([AB])\.weight', key)
    if m:
        layer = m.group(1)
        proj = m.group(2)
        ab = m.group(3).lower()
        new_key = f"layer_{layer}.{proj}.lora_{ab}.weight"
        converted[new_key] = tensor.float()
    else:
        skipped.append(key)

if skipped:
    print(f"Warning: {len(skipped)} unmatched keys skipped: {skipped[:3]}", file=sys.stderr)

if not converted:
    print("ERROR: zero keys converted — adapter format may have changed", file=sys.stderr)
    sys.exit(1)

save_file(converted, sys.argv[2])
print(f"Converted {len(converted)} keys")
"#;

    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(torchtune_adapter.as_os_str())
        .arg(candle_output.as_os_str())
        .output()
        .map_err(|e| format!("python3: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("adapter conversion failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("  {}", stdout.trim());

    let count = stdout
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    Ok(count)
}

/// Run the full training pipeline for a claimed job.
///
/// 1. Execute pre_hook
/// 2. Generate torchtune config from payload
/// 3. Run torchtune training
/// 4. Convert adapter to Candle format
/// 5. Execute post_hook (success) or on_failure (error)
///
/// Returns the path to the Candle-format adapter and duration.
pub fn run_training(job_id: &str, payload: &TrainingPayload) -> Result<RunResult, String> {
    let start = std::time::Instant::now();

    // Step 0: Execute pre_hook
    run_pre_hook(job_id, payload)?;

    let config = RunConfig::from_payload(payload);

    // Step 1: Generate torchtune config
    std::fs::create_dir_all(&config.output_dir).map_err(|e| format!("mkdir output: {e}"))?;
    let config_path = config.output_dir.join("torchtune_config.yaml");
    generate_torchtune_config(&config, &config_path)?;
    eprintln!("  Config generated: {:?}", config_path);

    // Step 2: Run training
    let training_result = run_torchtune(&config_path, &config.venv_path);

    match training_result {
        Ok(()) => {
            // Step 3: Find the final epoch adapter
            let epoch_dirs: Vec<_> = std::fs::read_dir(&config.output_dir)
                .map_err(|e| format!("readdir: {e}"))?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("epoch_"))
                .collect();

            let final_epoch = epoch_dirs
                .iter()
                .max_by_key(|e| e.file_name())
                .ok_or("No epoch directory found after training")?;

            let torchtune_adapter = final_epoch.path().join("adapter_model.safetensors");
            if !torchtune_adapter.exists() {
                let err = format!("Adapter not found at {:?}", torchtune_adapter);
                run_failure_hook(job_id, payload).ok();
                return Err(err);
            }

            // Step 4: Convert to Candle format
            let candle_adapter = config.output_dir.join("adapter_candle.safetensors");
            let key_count = convert_adapter_to_candle(&torchtune_adapter, &candle_adapter)?;

            if key_count == 0 {
                let err =
                    "Adapter conversion produced zero keys — format may have changed".to_string();
                run_failure_hook(job_id, payload).ok();
                return Err(err);
            }
            eprintln!("  Adapter: {key_count} LoRA keys converted to Candle format");

            let duration = start.elapsed();

            // Step 5: Execute post_hook
            run_post_hook(job_id, payload)?;

            Ok(RunResult {
                adapter_path: candle_adapter,
                duration_secs: duration.as_secs_f64(),
            })
        }
        Err(e) => {
            run_failure_hook(job_id, payload).ok();
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::training_queue::Priority;
    use tempfile::TempDir;

    fn test_payload() -> TrainingPayload {
        TrainingPayload {
            experiment_id: "EXPR-TEST".to_string(),
            being: "santiago-test".to_string(),
            corpus: "test".to_string(),
            base_model: Some("Qwen/Qwen2.5-3B-Instruct".to_string()),
            adapter_path: None,
            phases: Some(2),
            batch_size: Some(4),
            learning_rate: Some(3e-4),
            cq_threshold: None,
            ethical_ratio: None,
            quantize: Some(true),
            dual_loss: None,
            curriculum_path: Some("corpus/test/train.jsonl".to_string()),
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
            priority: Some(Priority::High),
            estimated_duration_min: Some(60),
            depends_on: None,
            output_dir: None,
        }
    }

    #[test]
    fn test_run_config_from_payload_defaults() {
        let payload = test_payload();
        let config = RunConfig::from_payload(&payload);
        assert_eq!(config.model_dir, PathBuf::from("Qwen/Qwen2.5-3B-Instruct"));
        assert_eq!(config.data_path, PathBuf::from("corpus/test/train.jsonl"));
        assert_eq!(config.epochs, 2);
        assert_eq!(config.batch_size, 4);
        assert!((config.learning_rate - 3e-4).abs() < 1e-10);
        assert!(config.quantize);
    }

    #[test]
    fn test_run_config_from_minimal_payload() {
        let payload = TrainingPayload {
            experiment_id: "EXPR-MIN".to_string(),
            being: "being".to_string(),
            corpus: "corpus".to_string(),
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
        let config = RunConfig::from_payload(&payload);
        assert_eq!(config.epochs, 3); // default
        assert_eq!(config.batch_size, 1); // default
        assert!(!config.quantize); // default
        assert_eq!(
            config.output_dir,
            PathBuf::from("research/shared/eval-data/being/EXPR-MIN")
        );
    }

    #[test]
    fn test_generate_torchtune_config() {
        let dir = TempDir::new().unwrap();
        let payload = test_payload();
        let mut config = RunConfig::from_payload(&payload);
        config.output_dir = dir.path().to_path_buf();
        let config_path = dir.path().join("config.yaml");

        generate_torchtune_config(&config, &config_path).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("lora_rank: 16"));
        assert!(content.contains("lora_alpha: 32"));
        assert!(content.contains("epochs: 2"));
        assert!(content.contains("Int4WeightOnlyQuantizer")); // QLoRA enabled
        assert!(content.contains("device: cuda"));
    }

    #[test]
    fn test_generate_torchtune_config_no_quantize() {
        let dir = TempDir::new().unwrap();
        let mut payload = test_payload();
        payload.quantize = None;
        let mut config = RunConfig::from_payload(&payload);
        config.output_dir = dir.path().to_path_buf();
        let config_path = dir.path().join("config.yaml");

        generate_torchtune_config(&config, &config_path).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(!content.contains("Int4WeightOnlyQuantizer"));
    }

    #[test]
    fn test_torchtune_not_found_error() {
        let dir = TempDir::new().unwrap();
        let venv = dir.path().join("nonexistent_venv");
        let result = run_torchtune(Path::new("config.yaml"), &venv);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("torchtune not found"));
    }

    #[test]
    fn test_run_config_custom_output_dir() {
        let mut payload = test_payload();
        payload.output_dir = Some("custom/output".to_string());
        let config = RunConfig::from_payload(&payload);
        assert_eq!(config.output_dir, PathBuf::from("custom/output"));
    }
}
