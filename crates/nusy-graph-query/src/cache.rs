//! Embedding cache — content-hash invalidation with Parquet persistence.
//!
//! Caches embedding vectors keyed by (id, content_hash). When content changes,
//! the hash changes and the cache entry is invalidated. Supports Parquet
//! persistence for warm starts across sessions.

use crate::embedding::{EmbeddedItem, EmbeddingError, EmbeddingProvider, Result};
use std::collections::HashMap;
use std::path::Path;

/// An embedding cache that avoids re-computing embeddings for unchanged content.
///
/// Keys are `(id, content_hash)` pairs. When the content hash changes, the
/// old embedding is invalidated and a new one is computed.
pub struct EmbeddingCache {
    /// Map from id → (content_hash, vector).
    entries: HashMap<String, CacheEntry>,
    /// Number of cache hits since creation.
    hits: u64,
    /// Number of cache misses since creation.
    misses: u64,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    content_hash: String,
    vector: Vec<f32>,
}

impl EmbeddingCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Cache hit count.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Cache miss count.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Look up an embedding by ID and content hash.
    ///
    /// Returns `Some(vector)` if the cache has a valid entry (matching hash).
    /// Returns `None` if the entry is missing or the hash has changed.
    pub fn get(&mut self, id: &str, content_hash: &str) -> Option<&Vec<f32>> {
        if let Some(entry) = self.entries.get(id)
            && entry.content_hash == content_hash
        {
            self.hits += 1;
            return Some(&entry.vector);
        }
        self.misses += 1;
        None
    }

    /// Insert or update a cache entry.
    pub fn insert(&mut self, id: String, content_hash: String, vector: Vec<f32>) {
        self.entries.insert(
            id,
            CacheEntry {
                content_hash,
                vector,
            },
        );
    }

    /// Embed items using the cache, only computing embeddings for cache misses.
    ///
    /// `items` is a list of `(id, content_hash, text)` tuples. Items with
    /// unchanged content hashes use cached vectors. Changed items are embedded
    /// in batch and cached.
    pub fn embed_cached(
        &mut self,
        items: &[(String, String, String)],
        provider: &dyn EmbeddingProvider,
    ) -> Result<Vec<EmbeddedItem>> {
        let mut results = Vec::with_capacity(items.len());
        let mut to_embed: Vec<(usize, String, String, String)> = Vec::new();

        for (idx, (id, hash, text)) in items.iter().enumerate() {
            if let Some(vec) = self.get(id, hash) {
                results.push((
                    idx,
                    EmbeddedItem {
                        id: id.clone(),
                        vector: vec.clone(),
                    },
                ));
            } else {
                to_embed.push((idx, id.clone(), hash.clone(), text.clone()));
            }
        }

        if !to_embed.is_empty() {
            let texts: Vec<String> = to_embed.iter().map(|(_, _, _, t)| t.clone()).collect();
            let vectors = provider.embed_batch(&texts)?;

            for (vec, (idx, id, hash, _)) in vectors.into_iter().zip(to_embed) {
                if vec.len() != provider.dim() {
                    return Err(EmbeddingError::DimensionMismatch {
                        expected: provider.dim(),
                        actual: vec.len(),
                    });
                }
                self.insert(id.clone(), hash, vec.clone());
                results.push((idx, EmbeddedItem { id, vector: vec }));
            }
        }

        // Sort by original index to maintain input order
        results.sort_by_key(|(idx, _)| *idx);
        Ok(results.into_iter().map(|(_, item)| item).collect())
    }

    /// Save the cache to a Parquet file.
    pub fn save(&self, path: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use arrow::array::{FixedSizeListArray, Float32Array, RecordBatch, StringArray};
        use arrow::buffer::{BooleanBuffer, NullBuffer};
        use arrow::datatypes::{DataType, Field, Schema};
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;

        if self.entries.is_empty() {
            return Ok(());
        }

        // Determine embedding dimension from first entry
        let dim = self
            .entries
            .values()
            .next()
            .map(|e| e.vector.len())
            .unwrap_or(0);
        if dim == 0 {
            return Ok(());
        }

        let n = self.entries.len();
        let mut ids = Vec::with_capacity(n);
        let mut hashes = Vec::with_capacity(n);
        let mut flat_vectors = Vec::with_capacity(n * dim);
        let mut validity = Vec::with_capacity(n);

        for (id, entry) in &self.entries {
            ids.push(id.as_str());
            hashes.push(entry.content_hash.as_str());
            flat_vectors.extend_from_slice(&entry.vector);
            validity.push(true);
        }

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("content_hash", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, false)),
                    dim as i32,
                ),
                false,
            ),
        ]));

        let embedding_array = FixedSizeListArray::try_new(
            Arc::new(Field::new("item", DataType::Float32, false)),
            dim as i32,
            Arc::new(Float32Array::from(flat_vectors)),
            Some(NullBuffer::new(BooleanBuffer::from(validity))),
        )?;

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(ids)),
                Arc::new(StringArray::from(hashes)),
                Arc::new(embedding_array),
            ],
        )?;

        let file = std::fs::File::create(path)?;
        let mut writer = ArrowWriter::try_new(file, batch.schema(), None)?;
        writer.write(&batch)?;
        writer.close()?;

        Ok(())
    }

    /// Load a cache from a Parquet file.
    pub fn load(path: &Path) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        use arrow::array::{Array, FixedSizeListArray, Float32Array, StringArray};
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

        if !path.exists() {
            return Ok(Self::new());
        }

        let file = std::fs::File::open(path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let reader = builder.build()?;

        let mut cache = Self::new();

        for batch in reader {
            let batch = batch?;
            let ids = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("id column not StringArray")?;
            let hashes = batch
                .column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("hash column not StringArray")?;
            let vectors = batch
                .column(2)
                .as_any()
                .downcast_ref::<FixedSizeListArray>()
                .ok_or("vector column not FixedSizeListArray")?;

            for i in 0..batch.num_rows() {
                if vectors.is_null(i) {
                    continue;
                }
                let values = vectors.value(i);
                let float_arr = values
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .ok_or("vector values not Float32Array")?;
                let vec: Vec<f32> = (0..float_arr.len()).map(|j| float_arr.value(j)).collect();

                cache.insert(ids.value(i).to_string(), hashes.value(i).to_string(), vec);
            }
        }

        Ok(cache)
    }
}

impl Default for EmbeddingCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::HashEmbeddingProvider;

    #[test]
    fn test_cache_hit() {
        let mut cache = EmbeddingCache::new();
        cache.insert("item-1".to_string(), "hash-a".to_string(), vec![1.0, 2.0]);

        assert!(cache.get("item-1", "hash-a").is_some());
        assert_eq!(cache.hits(), 1);
    }

    #[test]
    fn test_cache_miss_wrong_hash() {
        let mut cache = EmbeddingCache::new();
        cache.insert("item-1".to_string(), "hash-a".to_string(), vec![1.0, 2.0]);

        assert!(cache.get("item-1", "hash-b").is_none());
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn test_cache_miss_not_found() {
        let mut cache = EmbeddingCache::new();
        assert!(cache.get("nonexistent", "hash").is_none());
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn test_embed_cached() {
        let p = HashEmbeddingProvider::new(64);
        let mut cache = EmbeddingCache::new();

        let items = vec![
            ("A".to_string(), "h1".to_string(), "hello".to_string()),
            ("B".to_string(), "h2".to_string(), "world".to_string()),
        ];

        // First call — all misses
        let result = cache.embed_cached(&items, &p).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(cache.misses(), 2);
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.len(), 2);

        // Second call — all hits
        let result2 = cache.embed_cached(&items, &p).unwrap();
        assert_eq!(result2.len(), 2);
        assert_eq!(cache.hits(), 2);
        assert_eq!(result[0].vector, result2[0].vector);
    }

    #[test]
    fn test_embed_cached_invalidation() {
        let p = HashEmbeddingProvider::new(64);
        let mut cache = EmbeddingCache::new();

        let items1 = vec![("A".to_string(), "h1".to_string(), "hello".to_string())];
        let r1 = cache.embed_cached(&items1, &p).unwrap();

        // Change content hash → cache miss
        let items2 = vec![("A".to_string(), "h2".to_string(), "goodbye".to_string())];
        let r2 = cache.embed_cached(&items2, &p).unwrap();

        assert_ne!(r1[0].vector, r2[0].vector);
        assert_eq!(cache.len(), 1); // still one entry (updated)
    }

    #[test]
    fn test_cache_save_load_round_trip() {
        let p = HashEmbeddingProvider::new(64);
        let mut cache = EmbeddingCache::new();

        let items = vec![
            ("A".to_string(), "h1".to_string(), "hello".to_string()),
            ("B".to_string(), "h2".to_string(), "world".to_string()),
        ];
        cache.embed_cached(&items, &p).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.parquet");

        cache.save(&path).unwrap();
        assert!(path.exists());

        let loaded = EmbeddingCache::load(&path).unwrap();
        assert_eq!(loaded.len(), 2);

        // Verify vectors survived round-trip
        let mut loaded = loaded;
        assert!(loaded.get("A", "h1").is_some());
        assert!(loaded.get("B", "h2").is_some());
    }

    #[test]
    fn test_cache_load_nonexistent() {
        let path = Path::new("/tmp/nonexistent-cache-12345.parquet");
        let cache = EmbeddingCache::load(path).unwrap();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_empty_save() {
        let cache = EmbeddingCache::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.parquet");

        // Empty cache save should succeed without creating a file
        cache.save(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn test_embed_cached_preserves_order() {
        let p = HashEmbeddingProvider::new(64);
        let mut cache = EmbeddingCache::new();

        // Pre-cache "B" but not "A"
        cache.insert("B".to_string(), "h2".to_string(), p.embed("world").unwrap());

        let items = vec![
            ("A".to_string(), "h1".to_string(), "hello".to_string()),
            ("B".to_string(), "h2".to_string(), "world".to_string()),
            ("C".to_string(), "h3".to_string(), "test".to_string()),
        ];

        let result = cache.embed_cached(&items, &p).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].id, "A"); // order preserved
        assert_eq!(result[1].id, "B");
        assert_eq!(result[2].id, "C");
    }
}
