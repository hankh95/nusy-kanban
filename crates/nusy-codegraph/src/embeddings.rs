//! Embedding generation and semantic search for code objects.
//!
//! Re-exports shared types from `nusy-graph-query` and adds codegraph-specific
//! functions for embedding CodeNodes and attaching embeddings to RecordBatches.

use crate::schema::{CODE_EMBEDDING_DIM, CodeNode, CodeNodeKind};
use arrow::array::{Array, Float32Array, RecordBatch};
use std::sync::Arc;

// Re-export shared types so existing consumers don't break.
pub use nusy_graph_query::embedding::{EmbeddingError, EmbeddingProvider, cosine_similarity};

pub type Result<T> = std::result::Result<T, EmbeddingError>;

/// Deterministic hash-based embedding provider for codegraph (768-dim).
pub struct HashEmbeddingProvider;

impl EmbeddingProvider for HashEmbeddingProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| nusy_graph_query::hash_to_vector(t, CODE_EMBEDDING_DIM as usize))
            .collect())
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(nusy_graph_query::hash_to_vector(
            text,
            CODE_EMBEDDING_DIM as usize,
        ))
    }

    fn dim(&self) -> usize {
        CODE_EMBEDDING_DIM as usize
    }
}

/// Build the embeddable text for a CodeNode.
///
/// Concatenates signature + docstring for functions/methods,
/// name + docstring for classes, docstring for modules.
pub fn node_to_embed_text(node: &CodeNode) -> Option<String> {
    let parts: Vec<&str> = [
        node.signature.as_deref(),
        node.docstring.as_deref(),
        Some(node.name.as_str()),
    ]
    .into_iter()
    .flatten()
    .collect();

    if parts.is_empty() || (parts.len() == 1 && parts[0] == node.name) {
        match node.kind {
            CodeNodeKind::File | CodeNodeKind::Module => {
                if node.docstring.is_some() {
                    Some(parts.join(" "))
                } else {
                    None
                }
            }
            _ => None,
        }
    } else {
        Some(parts.join(" "))
    }
}

/// Embed all CodeNodes that have embeddable text.
///
/// Returns a map from node ID to embedding vector.
pub fn embed_nodes(
    nodes: &[CodeNode],
    provider: &dyn EmbeddingProvider,
) -> Result<Vec<(String, Vec<f32>)>> {
    let embeddable: Vec<(String, String)> = nodes
        .iter()
        .filter_map(|n| node_to_embed_text(n).map(|text| (n.id.clone(), text)))
        .collect();

    if embeddable.is_empty() {
        return Ok(Vec::new());
    }

    let texts: Vec<String> = embeddable.iter().map(|(_, t)| t.clone()).collect();
    let vectors = provider.embed_batch(&texts)?;

    for vec in &vectors {
        if vec.len() != provider.dim() {
            return Err(EmbeddingError::DimensionMismatch {
                expected: provider.dim(),
                actual: vec.len(),
            });
        }
    }

    Ok(embeddable
        .into_iter()
        .zip(vectors)
        .map(|((id, _), vec)| (id, vec))
        .collect())
}

/// Update a CodeNodes RecordBatch with embedding vectors.
///
/// Replaces the null embedding column with actual vectors for nodes
/// that have embeddings. Nodes without embeddings remain null.
pub fn attach_embeddings(
    batch: &RecordBatch,
    embeddings: &[(String, Vec<f32>)],
) -> Result<RecordBatch> {
    use arrow::array::{FixedSizeListArray, StringArray};
    use arrow::buffer::{BooleanBuffer, NullBuffer};
    use arrow::datatypes::{DataType, Field};

    let dim = CODE_EMBEDDING_DIM as usize;
    let n = batch.num_rows();

    let embed_map: std::collections::HashMap<&str, &Vec<f32>> = embeddings
        .iter()
        .map(|(id, vec)| (id.as_str(), vec))
        .collect();

    let ids = batch
        .column(crate::schema::node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| EmbeddingError::Provider("id column is not StringArray".to_string()))?;

    let mut values = Vec::with_capacity(n * dim);
    let mut validity = Vec::with_capacity(n);

    for i in 0..n {
        let id = ids.value(i);
        if let Some(vec) = embed_map.get(id) {
            values.extend_from_slice(vec);
            validity.push(true);
        } else {
            values.extend(std::iter::repeat_n(0.0f32, dim));
            validity.push(false);
        }
    }

    let embedding_field = Arc::new(Field::new("item", DataType::Float32, false));
    let embedding_array = FixedSizeListArray::try_new(
        embedding_field,
        CODE_EMBEDDING_DIM,
        Arc::new(Float32Array::from(values)),
        Some(NullBuffer::new(BooleanBuffer::from(validity))),
    )
    .map_err(|e| EmbeddingError::Provider(e.to_string()))?;

    let mut columns: Vec<Arc<dyn Array>> = Vec::new();
    for col_idx in 0..batch.num_columns() {
        if col_idx == crate::schema::node_col::EMBEDDING {
            columns.push(Arc::new(embedding_array.clone()));
        } else {
            columns.push(batch.column(col_idx).clone());
        }
    }

    RecordBatch::try_new(batch.schema(), columns)
        .map_err(|e| EmbeddingError::Provider(e.to_string()))
}

/// A search result from semantic search.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// The CodeNode ID.
    pub id: String,
    /// The node name.
    pub name: String,
    /// The node kind.
    pub kind: CodeNodeKind,
    /// Cosine similarity score (0.0 to 1.0 for unit vectors).
    pub score: f32,
}

/// Semantic search over embedded CodeNodes.
///
/// Embeds the query text, computes cosine similarity against all
/// embedded nodes, and returns the top-k results.
pub fn semantic_search(
    nodes: &[CodeNode],
    embeddings: &[(String, Vec<f32>)],
    query: &str,
    provider: &dyn EmbeddingProvider,
    top_k: usize,
) -> Result<Vec<SearchResult>> {
    let query_vec = provider.embed(query)?;

    let mut results: Vec<SearchResult> = embeddings
        .iter()
        .filter_map(|(id, vec)| {
            let score = cosine_similarity(&query_vec, vec);
            let node = nodes.iter().find(|n| n.id == *id)?;
            Some(SearchResult {
                id: id.clone(),
                name: node.name.clone(),
                kind: node.kind,
                score,
            })
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

    fn sample_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "func:brain/signal.py::fuse".to_string(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "fuse".to_string(),
                signature: Some("def fuse(signals: list) -> dict".to_string()),
                docstring: Some("Fuse signals from multiple sources.".to_string()),
                body_hash: None,
                body: None,
                loc: Some(20),
                cyclomatic_complexity: Some(5),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:brain/train.py::train_lora".to_string(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "train_lora".to_string(),
                signature: Some("def train_lora(model, data) -> None".to_string()),
                docstring: Some("Train a LoRA adapter on the model.".to_string()),
                body_hash: None,
                body: None,
                loc: Some(50),
                cyclomatic_complexity: Some(8),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "class:brain/store.py::Store".to_string(),
                kind: CodeNodeKind::Class,
                parent_id: None,
                name: "Store".to_string(),
                signature: Some("class Store".to_string()),
                docstring: Some("Knowledge store for persisting graph data.".to_string()),
                body_hash: None,
                body: None,
                loc: Some(100),
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "file:brain/empty.py".to_string(),
                kind: CodeNodeKind::File,
                parent_id: None,
                name: "empty.py".to_string(),
                signature: None,
                docstring: None,
                body_hash: None,
                body: None,
                loc: Some(1),
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    #[test]
    fn test_hash_embedding_provider_deterministic() {
        let provider = HashEmbeddingProvider;
        let v1 = provider.embed("hello world").unwrap();
        let v2 = provider.embed("hello world").unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1.len(), CODE_EMBEDDING_DIM as usize);
    }

    #[test]
    fn test_hash_embedding_unit_length() {
        let provider = HashEmbeddingProvider;
        let v = provider.embed("test input").unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "Vector should be unit length, got norm={norm}"
        );
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
    fn test_node_to_embed_text() {
        let nodes = sample_nodes();
        let text = node_to_embed_text(&nodes[0]).expect("should embed");
        assert!(text.contains("fuse"));
        assert!(text.contains("signals"));

        let text = node_to_embed_text(&nodes[3]);
        assert!(text.is_none(), "File without docstring should not embed");
    }

    #[test]
    fn test_embed_nodes() {
        let nodes = sample_nodes();
        let provider = HashEmbeddingProvider;
        let embeddings = embed_nodes(&nodes, &provider).unwrap();

        assert_eq!(embeddings.len(), 3);
        for (_, vec) in &embeddings {
            assert_eq!(vec.len(), CODE_EMBEDDING_DIM as usize);
        }
    }

    #[test]
    fn test_semantic_search() {
        let nodes = sample_nodes();
        let provider = HashEmbeddingProvider;
        let embeddings = embed_nodes(&nodes, &provider).unwrap();

        let results = semantic_search(&nodes, &embeddings, "signal fusion", &provider, 3).unwrap();

        assert!(!results.is_empty());
        assert!(results.len() <= 3);

        for r in &results {
            assert!(r.score >= -1.0 && r.score <= 1.0);
        }
        for w in results.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
    }

    #[test]
    fn test_attach_embeddings_to_batch() {
        use crate::schema::build_code_nodes_batch;

        let nodes = sample_nodes();
        let batch = build_code_nodes_batch(&nodes).expect("build batch");

        let emb_col = batch.column(crate::schema::node_col::EMBEDDING);
        for i in 0..batch.num_rows() {
            assert!(emb_col.is_null(i), "Row {i} should be null initially");
        }

        let provider = HashEmbeddingProvider;
        let embeddings = embed_nodes(&nodes, &provider).unwrap();
        let updated = attach_embeddings(&batch, &embeddings).expect("attach");

        assert_eq!(updated.num_rows(), batch.num_rows());
        assert_eq!(updated.num_columns(), batch.num_columns());

        let emb_col = updated.column(crate::schema::node_col::EMBEDDING);
        assert!(!emb_col.is_null(0), "fuse should be embedded");
        assert!(emb_col.is_null(3), "file without docstring should be null");
    }

    #[test]
    fn test_embed_batch_consistency() {
        let provider = HashEmbeddingProvider;
        let texts = vec!["hello".to_string(), "world".to_string()];
        let batch_result = provider.embed_batch(&texts).unwrap();
        let single_1 = provider.embed("hello").unwrap();
        let single_2 = provider.embed("world").unwrap();
        assert_eq!(batch_result[0], single_1);
        assert_eq!(batch_result[1], single_2);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn test_cosine_similarity_length_mismatch() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }
}
