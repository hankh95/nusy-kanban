//! Subprocess embedding provider — shells to Python sentence-transformers.
//!
//! Fallback for machines without Ollama. Spawns a Python process that
//! loads a sentence-transformers model and encodes text to embeddings.
//!
//! Feature-gated behind `subprocess`.
//!
//! ```toml
//! [dependencies]
//! nusy-graph-query = { version = "0.14", features = ["subprocess"] }
//! ```

use crate::embedding::{EmbeddingError, EmbeddingProvider, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// Embedding provider that shells to Python sentence-transformers.
///
/// Spawns a Python subprocess for each batch. The subprocess loads the
/// model, encodes all texts, and returns JSON embeddings on stdout.
///
/// Requires: `pip install sentence-transformers`
pub struct SubprocessEmbeddingProvider {
    /// Python executable (default: `python3`).
    python: String,
    /// Model name (default: `all-MiniLM-L6-v2`, 384-dim).
    model: String,
    /// Expected embedding dimension.
    dim: usize,
}

impl SubprocessEmbeddingProvider {
    /// Create a new subprocess provider with default settings.
    pub fn new() -> Self {
        Self {
            python: "python3".to_string(),
            model: "all-MiniLM-L6-v2".to_string(),
            dim: 384,
        }
    }

    /// Set the Python executable path.
    pub fn with_python(mut self, python: &str) -> Self {
        self.python = python.to_string();
        self
    }

    /// Set the sentence-transformers model.
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// Set the expected embedding dimension.
    pub fn with_dim(mut self, dim: usize) -> Self {
        self.dim = dim;
        self
    }
}

impl Default for SubprocessEmbeddingProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Python script that loads sentence-transformers and encodes texts.
///
/// Reads JSON array of strings from stdin, writes JSON array of vectors
/// to stdout. Model is passed as the first CLI argument.
const EMBED_SCRIPT: &str = r#"
import sys, json
model_name = sys.argv[1]
texts = json.loads(sys.stdin.read())
from sentence_transformers import SentenceTransformer
model = SentenceTransformer(model_name)
embeddings = model.encode(texts, normalize_embeddings=True)
json.dump(embeddings.tolist(), sys.stdout)
"#;

impl EmbeddingProvider for SubprocessEmbeddingProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let input_json = serde_json::to_string(texts)
            .map_err(|e| EmbeddingError::Provider(format!("Failed to serialize texts: {e}")))?;

        let mut child = Command::new(&self.python)
            .args(["-c", EMBED_SCRIPT, &self.model])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                EmbeddingError::Provider(format!(
                    "Failed to spawn Python ({}): {e}. Is sentence-transformers installed?",
                    self.python
                ))
            })?;

        // Write texts to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input_json.as_bytes()).map_err(|e| {
                EmbeddingError::Provider(format!("Failed to write to Python stdin: {e}"))
            })?;
        }

        let output = child
            .wait_with_output()
            .map_err(|e| EmbeddingError::Provider(format!("Python subprocess failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EmbeddingError::Provider(format!(
                "Python embedding failed (exit {}): {}",
                output.status,
                stderr.lines().last().unwrap_or(&stderr)
            )));
        }

        let stdout = String::from_utf8(output.stdout)
            .map_err(|e| EmbeddingError::Provider(format!("Invalid UTF-8 from Python: {e}")))?;

        let embeddings: Vec<Vec<f32>> = serde_json::from_str(&stdout)
            .map_err(|e| EmbeddingError::Provider(format!("Failed to parse Python output: {e}")))?;

        // Validate dimensions
        for (i, vec) in embeddings.iter().enumerate() {
            if vec.len() != self.dim {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: self.dim,
                    actual: vec.len(),
                });
            }
            let _ = i;
        }

        Ok(embeddings)
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subprocess_provider_default() {
        let provider = SubprocessEmbeddingProvider::new();
        assert_eq!(provider.dim(), 384);
        assert_eq!(provider.python, "python3");
        assert_eq!(provider.model, "all-MiniLM-L6-v2");
    }

    #[test]
    fn test_subprocess_provider_builder() {
        let provider = SubprocessEmbeddingProvider::new()
            .with_python("/usr/bin/python3")
            .with_model("all-mpnet-base-v2")
            .with_dim(768);

        assert_eq!(provider.python, "/usr/bin/python3");
        assert_eq!(provider.model, "all-mpnet-base-v2");
        assert_eq!(provider.dim(), 768);
    }

    #[test]
    fn test_subprocess_provider_empty_batch() {
        let provider = SubprocessEmbeddingProvider::new();
        let result = provider.embed_batch(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_subprocess_provider_bad_python() {
        let provider = SubprocessEmbeddingProvider::new().with_python("/nonexistent/python3");
        let result = provider.embed_batch(&["hello".to_string()]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Failed to spawn Python"));
    }

    // Integration test — only runs if sentence-transformers is installed
    // #[test]
    // fn test_subprocess_real_embedding() {
    //     let provider = SubprocessEmbeddingProvider::new();
    //     let result = provider.embed_batch(&["hello world".to_string()]);
    //     if let Ok(vecs) = result {
    //         assert_eq!(vecs.len(), 1);
    //         assert_eq!(vecs[0].len(), 384);
    //     }
    // }
}
