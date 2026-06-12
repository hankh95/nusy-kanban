//! fastembed-rs local embedding provider — ONNX-based, no network required.
//!
//! EX-3384: Replaces 50ms/chunk Ollama HTTP with ~2ms/chunk local ONNX inference.
//! Feature-gated behind `fastembed`.
//!
//! ```toml
//! [dependencies]
//! nusy-graph-query = { version = "0.14", features = ["fastembed"] }
//! ```

use crate::embedding::{EmbeddingError, EmbeddingProvider, Result};

/// Default fastembed model — Xenova/bge-small-en-v1.5 (384-dim, fast, good quality).
/// Note: fastembed uses Xenova's ONNX exports, not the original BAAI model IDs.
const DEFAULT_MODEL: &str = "Xenova/bge-small-en-v1.5";
const DEFAULT_DIM: usize = 384;

/// Local embedding provider using fastembed-rs (ONNX Runtime).
///
/// No network dependencies — downloads the model on first use, then runs
/// locally with ONNX Runtime. Batch-native for maximum throughput.
///
/// # Example
/// ```rust,ignore
/// use nusy_graph_query::fastembed_provider::FastembedProvider;
/// use nusy_graph_query::embedding::EmbeddingProvider;
///
/// let provider = FastembedProvider::new().unwrap();
/// let embeddings = provider.embed_batch(&["Hello".into(), "World".into()]).unwrap();
/// assert_eq!(embeddings.len(), 2);
/// assert_eq!(embeddings[0].len(), 384);
/// ```
pub struct FastembedProvider {
    model: std::sync::Mutex<fastembed::TextEmbedding>,
    dim: usize,
}

impl FastembedProvider {
    /// Create a provider with the default model (BAAI/bge-small-en-v1.5, 384-dim).
    ///
    /// SUB-EX-E: If `NUSY_FASTEMBED_MODEL_DIR` is set, loads the model from
    /// that directory without touching the network. The directory must
    /// contain `model.onnx` (or `onnx/model.onnx`), `tokenizer.json`,
    /// `config.json`, `special_tokens_map.json`, `tokenizer_config.json`.
    /// This is the only way to initialize fastembed in a China-offline
    /// environment where `huggingface.co` is unreachable.
    pub fn new() -> std::result::Result<Self, EmbeddingError> {
        if let Ok(dir) = std::env::var("NUSY_FASTEMBED_MODEL_DIR") {
            return Self::from_local_dir(dir, DEFAULT_DIM);
        }
        Self::with_model(DEFAULT_MODEL, DEFAULT_DIM)
    }

    /// Load a fastembed model from a local directory (no network access).
    ///
    /// Expected files in `dir`:
    /// - `model.onnx` or `onnx/model.onnx` — the ONNX model weights
    /// - `tokenizer.json`, `config.json`, `special_tokens_map.json`, `tokenizer_config.json`
    pub fn from_local_dir(
        dir: impl Into<std::path::PathBuf>,
        dim: usize,
    ) -> std::result::Result<Self, EmbeddingError> {
        let dir: std::path::PathBuf = dir.into();
        let read = |name: &str| -> std::result::Result<Vec<u8>, EmbeddingError> {
            let p1 = dir.join(name);
            let p2 = dir.join("onnx").join(name);
            let path = if p1.exists() { p1 } else { p2 };
            std::fs::read(&path).map_err(|e| {
                EmbeddingError::Provider(format!(
                    "fastembed local load: cannot read {}: {e}",
                    path.display()
                ))
            })
        };
        let onnx_bytes = read("model.onnx")?;
        let tokenizer_files = fastembed::TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };
        let user_model = fastembed::UserDefinedEmbeddingModel::new(onnx_bytes, tokenizer_files);
        let options = fastembed::InitOptionsUserDefined::new();
        let model = fastembed::TextEmbedding::try_new_from_user_defined(user_model, options)
            .map_err(|e| {
                EmbeddingError::Provider(format!(
                    "fastembed local init failed (dir={}): {e}",
                    dir.display()
                ))
            })?;
        Ok(Self {
            model: std::sync::Mutex::new(model),
            dim,
        })
    }

    /// Create a provider with a specific model.
    ///
    /// Supported models (see fastembed docs for full list):
    /// - `BAAI/bge-small-en-v1.5` (384-dim, fast, default)
    /// - `BAAI/bge-base-en-v1.5` (768-dim, balanced)
    /// - `BAAI/bge-large-en-v1.5` (1024-dim, highest quality)
    /// - `sentence-transformers/all-MiniLM-L6-v2` (384-dim)
    pub fn with_model(model_name: &str, dim: usize) -> std::result::Result<Self, EmbeddingError> {
        let model_info = fastembed::TextEmbedding::list_supported_models()
            .into_iter()
            .find(|m| m.model_code == model_name)
            .ok_or_else(|| {
                EmbeddingError::Provider(format!(
                    "Model '{}' not supported by fastembed. Use TextEmbedding::list_supported_models() to see available models.",
                    model_name
                ))
            })?;

        let options =
            fastembed::InitOptions::new(model_info.model).with_show_download_progress(true);

        let model = fastembed::TextEmbedding::try_new(options).map_err(|e| {
            EmbeddingError::Provider(format!("Failed to initialize fastembed model: {e}"))
        })?;

        Ok(Self {
            model: std::sync::Mutex::new(model),
            dim,
        })
    }

    /// Create a provider with a custom cache directory for model files.
    pub fn with_cache_dir(
        model_name: &str,
        dim: usize,
        cache_dir: impl Into<std::path::PathBuf>,
    ) -> std::result::Result<Self, EmbeddingError> {
        let model_info = fastembed::TextEmbedding::list_supported_models()
            .into_iter()
            .find(|m| m.model_code == model_name)
            .ok_or_else(|| {
                EmbeddingError::Provider(format!("Model '{}' not supported", model_name))
            })?;

        let options = fastembed::InitOptions::new(model_info.model)
            .with_show_download_progress(true)
            .with_cache_dir(cache_dir.into());

        let model = fastembed::TextEmbedding::try_new(options).map_err(|e| {
            EmbeddingError::Provider(format!("Failed to initialize fastembed model: {e}"))
        })?;

        Ok(Self {
            model: std::sync::Mutex::new(model),
            dim,
        })
    }
}

impl EmbeddingProvider for FastembedProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut model = self
            .model
            .lock()
            .map_err(|e| EmbeddingError::Provider(format!("fastembed lock poisoned: {e}")))?;
        let results = model
            .embed(texts, None)
            .map_err(|e| EmbeddingError::Provider(format!("fastembed embed failed: {e}")))?;

        // Validate dimensions.
        for (i, vec) in results.iter().enumerate() {
            if vec.len() != self.dim {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: self.dim,
                    actual: vec.len(),
                });
            }
            let _ = i; // suppress unused warning
        }

        Ok(results)
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires model download on first run (~30MB)
    fn test_fastembed_single() {
        let provider = FastembedProvider::new().expect("init should succeed");
        let vec = provider.embed("Hello world").expect("embed should succeed");
        assert_eq!(vec.len(), DEFAULT_DIM);

        // Should be a unit-ish vector (fastembed normalizes by default).
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.1,
            "expected ~unit vector, got norm={norm}"
        );
    }

    #[test]
    #[ignore] // Requires model download
    fn test_fastembed_batch() {
        let provider = FastembedProvider::new().expect("init should succeed");
        let texts = vec![
            "The cat sat on the mat".to_string(),
            "Dogs are loyal animals".to_string(),
            "Mathematics is beautiful".to_string(),
        ];
        let results = provider
            .embed_batch(&texts)
            .expect("batch embed should succeed");
        assert_eq!(results.len(), 3);
        for vec in &results {
            assert_eq!(vec.len(), DEFAULT_DIM);
        }
    }

    #[test]
    #[ignore] // Requires model download
    fn test_fastembed_similarity() {
        use crate::embedding::cosine_similarity;

        let provider = FastembedProvider::new().expect("init should succeed");
        let cat = provider.embed("cat").expect("embed");
        let dog = provider.embed("dog").expect("embed");
        let math = provider.embed("integral calculus").expect("embed");

        let cat_dog = cosine_similarity(&cat, &dog);
        let cat_math = cosine_similarity(&cat, &math);

        // cat-dog should be more similar than cat-math.
        assert!(
            cat_dog > cat_math,
            "cat-dog ({cat_dog}) should be more similar than cat-math ({cat_math})"
        );
    }

    #[test]
    fn test_from_local_dir_missing_files_yields_actionable_error() {
        // SUB-EX-E: with no files present, the error message must point at
        // the missing file path so an operator can fix the staging.
        let tmp = std::env::temp_dir().join("nusy_fastembed_subex_e_missing");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let err = FastembedProvider::from_local_dir(&tmp, DEFAULT_DIM)
            .err()
            .expect("should fail without model.onnx");
        let msg = format!("{err}");
        assert!(
            msg.contains("model.onnx") && msg.contains("cannot read"),
            "error must name the missing file: {msg}"
        );
    }

    #[test]
    #[ignore] // Requires model download
    fn test_fastembed_empty_batch() {
        let provider = FastembedProvider::new().expect("init should succeed");
        let results = provider
            .embed_batch(&[])
            .expect("empty batch should succeed");
        assert!(results.is_empty());
    }
}
