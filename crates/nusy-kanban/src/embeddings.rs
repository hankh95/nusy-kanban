//! Embedding infrastructure for kanban semantic search.
//!
//! Re-exports shared types from `nusy-graph-query` and adds kanban-specific
//! functions for embedding work items (title + tags + type as text).

use crate::schema::items_col;
use arrow::array::{Array, BooleanArray, ListArray, RecordBatch, StringArray};

/// Default embedding dimension (matches sentence-transformers MiniLM-L6-v2).
pub const KANBAN_EMBEDDING_DIM: usize = 384;

// Re-export shared types so existing consumers don't break.
pub use nusy_graph_query::embedding::{
    EmbeddedItem, EmbeddingError, EmbeddingProvider, Result, SearchResult, cosine_similarity,
    semantic_search,
};

/// Deterministic hash-based embedding provider for kanban (384-dim).
pub struct HashEmbeddingProvider;

impl EmbeddingProvider for HashEmbeddingProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| nusy_graph_query::hash_to_vector(t, KANBAN_EMBEDDING_DIM))
            .collect())
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(nusy_graph_query::hash_to_vector(text, KANBAN_EMBEDDING_DIM))
    }

    fn dim(&self) -> usize {
        KANBAN_EMBEDDING_DIM
    }
}

/// Resolve an embedding provider by name.
///
/// Priority: explicit `provider_name` > `NUSY_EMBEDDING_PROVIDER` env var > "hash".
///
/// Supported providers:
/// - `"hash"` — deterministic hash-based (default, for testing/CI)
/// - `"subprocess"` — Python sentence-transformers (requires pip install)
/// - `"fastembed"` — Local ONNX via fastembed-rs (~2ms/chunk, no network)
pub fn resolve_provider(provider_name: Option<&str>) -> Box<dyn EmbeddingProvider> {
    let name = provider_name
        .map(|s| s.to_string())
        .or_else(|| std::env::var("NUSY_EMBEDDING_PROVIDER").ok())
        .unwrap_or_else(|| "hash".to_string());

    match name.as_str() {
        "subprocess" => Box::new(nusy_graph_query::SubprocessEmbeddingProvider::new()),
        #[cfg(feature = "fastembed")]
        "fastembed" => match nusy_graph_query::FastembedProvider::new() {
            Ok(provider) => Box::new(provider),
            Err(e) => {
                eprintln!("Warning: fastembed init failed ({e}), falling back to hash");
                Box::new(HashEmbeddingProvider)
            }
        },
        _ => Box::new(HashEmbeddingProvider),
    }
}

/// Build embeddable text for a kanban item: "title tag1 tag2 ... item_type".
fn item_embed_text(
    titles: &StringArray,
    types: &StringArray,
    tags: &ListArray,
    row: usize,
) -> String {
    let mut parts = vec![titles.value(row).to_string()];

    if !tags.is_null(row) {
        let tag_values = tags
            .value(row)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("tag string array")
            .clone();
        for i in 0..tag_values.len() {
            if !tag_values.is_null(i) {
                parts.push(tag_values.value(i).to_string());
            }
        }
    }

    parts.push(types.value(row).to_string());
    parts.join(" ")
}

/// Embed all non-deleted items from record batches.
pub fn embed_items(
    batches: &[RecordBatch],
    provider: &dyn EmbeddingProvider,
) -> Result<Vec<EmbeddedItem>> {
    let mut texts = Vec::new();
    let mut ids = Vec::new();

    for batch in batches {
        let id_col = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id column");
        let title_col = batch
            .column(items_col::TITLE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("title column");
        let type_col = batch
            .column(items_col::ITEM_TYPE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("type column");
        let tags_col = batch
            .column(items_col::TAGS)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("tags column");
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted column");

        for i in 0..batch.num_rows() {
            if !deleted.value(i) {
                ids.push(id_col.value(i).to_string());
                texts.push(item_embed_text(title_col, type_col, tags_col, i));
            }
        }
    }

    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let vectors = provider.embed_batch(&texts)?;

    for vec in &vectors {
        if vec.len() != provider.dim() {
            return Err(EmbeddingError::DimensionMismatch {
                expected: provider.dim(),
                actual: vec.len(),
            });
        }
    }

    Ok(ids
        .into_iter()
        .zip(vectors)
        .map(|(id, vector)| EmbeddedItem { id, vector })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crud::{CreateItemInput, KanbanStore};
    use crate::item_type::ItemType;

    #[test]
    fn test_hash_embedding_deterministic() {
        let provider = HashEmbeddingProvider;
        let v1 = provider.embed("hello world").unwrap();
        let v2 = provider.embed("hello world").unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1.len(), KANBAN_EMBEDDING_DIM);
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
    fn test_hash_embedding_different_inputs_differ() {
        let provider = HashEmbeddingProvider;
        let v1 = provider.embed("arrow kanban").unwrap();
        let v2 = provider.embed("signal fusion").unwrap();
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
    fn test_embed_items() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Arrow-Kanban Engine".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: Some("M5".to_string()),
                tags: vec!["v14".to_string(), "arrow".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        store
            .create_item(&CreateItemInput {
                title: "Fix signal fusion tests".to_string(),
                item_type: ItemType::Chore,
                priority: Some("medium".to_string()),
                assignee: None,
                tags: vec!["testing".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        let provider = HashEmbeddingProvider;
        let embeddings = embed_items(store.items_batches(), &provider).unwrap();

        assert_eq!(embeddings.len(), 2);
        for item in &embeddings {
            assert_eq!(item.vector.len(), KANBAN_EMBEDDING_DIM);
        }
    }

    #[test]
    fn test_semantic_search_returns_ranked_results() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Arrow-Kanban Engine".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec!["arrow".to_string(), "rust".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        store
            .create_item(&CreateItemInput {
                title: "Signal Fusion Pipeline".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("medium".to_string()),
                assignee: None,
                tags: vec!["signal".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        store
            .create_item(&CreateItemInput {
                title: "Fix broken tests".to_string(),
                item_type: ItemType::Chore,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        let provider = HashEmbeddingProvider;
        let embeddings = embed_items(store.items_batches(), &provider).unwrap();
        let results = semantic_search(&embeddings, "arrow kanban", &provider, 3).unwrap();

        assert_eq!(results.len(), 3);
        for w in results.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
        for r in &results {
            assert!(r.score >= -1.0 && r.score <= 1.0);
        }
    }

    #[test]
    fn test_semantic_search_top_k_limit() {
        let mut store = KanbanStore::new();
        for i in 0..10 {
            store
                .create_item(&CreateItemInput {
                    title: format!("Item {i}"),
                    item_type: ItemType::Chore,
                    priority: None,
                    assignee: None,
                    tags: vec![],
                    related: vec![],
                    depends_on: vec![],
                    body: None,
                })
                .expect("create");
        }

        let provider = HashEmbeddingProvider;
        let embeddings = embed_items(store.items_batches(), &provider).unwrap();
        let results = semantic_search(&embeddings, "test", &provider, 5).unwrap();
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_embed_items_empty_store() {
        let store = KanbanStore::new();
        let provider = HashEmbeddingProvider;
        let embeddings = embed_items(store.items_batches(), &provider).unwrap();
        assert!(embeddings.is_empty());
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
}
