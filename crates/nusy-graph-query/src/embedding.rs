//! Embedding infrastructure — provider trait, hash provider, cosine similarity.
//!
//! Shared between nusy-kanban (semantic search over work items) and
//! nusy-codegraph (semantic search over code objects). The embedding
//! dimension is configurable per consumer.

/// Errors from embedding operations.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("Embedding dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("Provider error: {0}")]
    Provider(String),
}

pub type Result<T> = std::result::Result<T, EmbeddingError>;

/// Trait for embedding providers.
///
/// Implementations can use local models (ONNX, sentence-transformers),
/// remote APIs (Ollama, OpenAI), or deterministic hashing (for testing).
pub trait EmbeddingProvider {
    /// Embed a batch of text strings into vectors.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Embed a single text string.
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let results = self.embed_batch(&[text.to_string()])?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::Provider("empty result".to_string()))
    }

    /// The embedding dimension this provider produces.
    fn dim(&self) -> usize;
}

/// Deterministic hash-based embedding provider for testing.
///
/// Produces reproducible unit-length vectors by hashing the input text.
/// Not semantically meaningful but stable across runs, making tests
/// deterministic.
pub struct HashEmbeddingProvider {
    dim: usize,
}

impl HashEmbeddingProvider {
    /// Create a hash embedding provider with the given dimension.
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl EmbeddingProvider for HashEmbeddingProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| hash_to_vector(t, self.dim)).collect())
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

/// Produce a deterministic unit-length vector from a text hash.
///
/// Uses SHA-256 chaining to generate enough bytes, then normalizes
/// to unit length. The same input always produces the same output.
pub fn hash_to_vector(text: &str, dim: usize) -> Vec<f32> {
    use sha2::{Digest, Sha256};
    let mut vec = Vec::with_capacity(dim);

    // Generate enough hash bytes to fill the vector.
    // Each SHA-256 gives 32 bytes -> 8 floats. Chain hashes for longer vectors.
    let mut seed = text.to_string();
    while vec.len() < dim {
        let mut hasher = Sha256::new();
        hasher.update(seed.as_bytes());
        let hash = hasher.finalize();
        for chunk in hash.chunks(4) {
            if vec.len() >= dim {
                break;
            }
            let bytes: [u8; 4] = chunk.try_into().expect("4 bytes from sha256 chunk");
            // Map to [-1, 1] range
            let val = (u32::from_le_bytes(bytes) as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32;
            vec.push(val);
        }
        seed = format!("{seed}+");
    }

    // Normalize to unit length
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut vec {
            *v /= norm;
        }
    }

    vec
}

/// Cosine similarity between two vectors.
///
/// Returns 0.0 for empty vectors or dimension mismatches.
/// For unit-length vectors, this is equivalent to the dot product.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

/// An embedded item — ID + vector pair.
///
/// Generic container used by both kanban (items) and codegraph (nodes).
#[derive(Debug, Clone)]
pub struct EmbeddedItem {
    pub id: String,
    pub vector: Vec<f32>,
}

/// A semantic search result — ID + similarity score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
}

/// Semantic search over a collection of embedded items.
///
/// Embeds the query text, computes cosine similarity against all items,
/// and returns the top-k results sorted by score descending.
pub fn semantic_search(
    embeddings: &[EmbeddedItem],
    query: &str,
    provider: &dyn EmbeddingProvider,
    top_k: usize,
) -> Result<Vec<SearchResult>> {
    let query_vec = provider.embed(query)?;

    let mut results: Vec<SearchResult> = embeddings
        .iter()
        .map(|item| SearchResult {
            id: item.id.clone(),
            score: cosine_similarity(&query_vec, &item.vector),
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(dim: usize) -> HashEmbeddingProvider {
        HashEmbeddingProvider::new(dim)
    }

    #[test]
    fn test_hash_embedding_deterministic() {
        let p = provider(384);
        let v1 = p.embed("hello world").unwrap();
        let v2 = p.embed("hello world").unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1.len(), 384);
    }

    #[test]
    fn test_hash_embedding_configurable_dim() {
        let p384 = provider(384);
        let p768 = provider(768);
        assert_eq!(p384.embed("test").unwrap().len(), 384);
        assert_eq!(p768.embed("test").unwrap().len(), 768);
    }

    #[test]
    fn test_hash_embedding_unit_length() {
        let p = provider(384);
        let v = p.embed("test input").unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "Vector should be unit length, got norm={norm}"
        );
    }

    #[test]
    fn test_hash_embedding_different_inputs_differ() {
        let p = provider(384);
        let v1 = p.embed("arrow kanban").unwrap();
        let v2 = p.embed("signal fusion").unwrap();
        assert_ne!(v1, v2);
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn test_cosine_similarity_length_mismatch() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn test_embed_batch_consistency() {
        let p = provider(384);
        let texts = vec!["hello".to_string(), "world".to_string()];
        let batch_result = p.embed_batch(&texts).unwrap();
        let single_1 = p.embed("hello").unwrap();
        let single_2 = p.embed("world").unwrap();
        assert_eq!(batch_result[0], single_1);
        assert_eq!(batch_result[1], single_2);
    }

    #[test]
    fn test_semantic_search_ranked() {
        let p = provider(384);
        let items: Vec<EmbeddedItem> = ["arrow kanban", "signal fusion", "graph query"]
            .iter()
            .map(|text| EmbeddedItem {
                id: text.to_string(),
                vector: p.embed(text).unwrap(),
            })
            .collect();

        let results = semantic_search(&items, "arrow", &p, 3).unwrap();
        assert_eq!(results.len(), 3);
        // Sorted descending by score
        for w in results.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
    }

    #[test]
    fn test_semantic_search_top_k() {
        let p = provider(384);
        let items: Vec<EmbeddedItem> = (0..10)
            .map(|i| EmbeddedItem {
                id: format!("item-{i}"),
                vector: p.embed(&format!("item {i}")).unwrap(),
            })
            .collect();

        let results = semantic_search(&items, "test", &p, 3).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_semantic_search_empty() {
        let p = provider(384);
        let results = semantic_search(&[], "test", &p, 10).unwrap();
        assert!(results.is_empty());
    }
}
