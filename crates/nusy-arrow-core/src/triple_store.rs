//! SimpleTripleStore — lightweight Arrow-native triple store.
//!
//! Replacement for Python `brain/reasoning/simple_triple_store.py`.
//! Wraps [`ArrowGraphStore`] with a simplified API that doesn't require
//! explicit namespace/Y-layer on every operation.
//!
//! # Quick Start
//!
//! ```rust
//! use nusy_arrow_core::triple_store::SimpleTripleStore;
//!
//! let mut store = SimpleTripleStore::new();
//! let id = store.add("Alice", "knows", "Bob", 0.9, "test").unwrap();
//! assert_eq!(store.count(None, None, None), 1);
//!
//! let results = store.query(Some("Alice"), None, None).unwrap();
//! assert_eq!(results.len(), 1);
//! assert_eq!(results[0].subject, "Alice");
//!
//! store.remove(&id).unwrap();
//! assert_eq!(store.count(None, None, None), 0);
//! ```

use crate::namespace::Namespace;
use crate::schema::col;
use crate::store::{ArrowGraphStore, QuerySpec, StoreError, Triple};
use crate::y_layer::YLayer;

use arrow::array::{Array, Float64Array, RecordBatch, StringArray};
use std::collections::HashMap;

/// Default namespace for SimpleTripleStore operations.
const DEFAULT_NAMESPACE: Namespace = Namespace::World;
/// Default Y-layer for SimpleTripleStore operations.
const DEFAULT_YLAYER: YLayer = YLayer::Semantic;

/// A retrieved triple with all metadata.
#[derive(Debug, Clone)]
pub struct StoredTriple {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub graph: Option<String>,
    pub confidence: f64,
    pub source: Option<String>,
}

/// Statistics about the store.
#[derive(Debug, Clone)]
pub struct StoreStats {
    pub total_triples: usize,
    pub unique_subjects: usize,
    pub unique_predicates: usize,
    pub unique_objects: usize,
    pub by_source: HashMap<String, usize>,
}

/// Lightweight Arrow-native triple store.
///
/// Wraps `ArrowGraphStore` with defaults for namespace and Y-layer,
/// providing the same API surface as Python's `SimpleTripleStore`.
pub struct SimpleTripleStore {
    inner: ArrowGraphStore,
    namespace: Namespace,
    y_layer: YLayer,
}

impl SimpleTripleStore {
    /// Create a new empty store with default namespace (World) and Y-layer (Semantic).
    pub fn new() -> Self {
        Self {
            inner: ArrowGraphStore::new(),
            namespace: DEFAULT_NAMESPACE,
            y_layer: DEFAULT_YLAYER,
        }
    }

    /// Create with custom namespace and Y-layer defaults.
    pub fn with_defaults(namespace: Namespace, y_layer: YLayer) -> Self {
        Self {
            inner: ArrowGraphStore::new(),
            namespace,
            y_layer,
        }
    }

    /// Add a triple. Returns the generated triple ID.
    pub fn add(
        &mut self,
        subject: &str,
        predicate: &str,
        object: &str,
        confidence: f64,
        source: &str,
    ) -> Result<String, StoreError> {
        let triple = Triple {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            graph: None,
            confidence: Some(confidence),
            source_document: Some(source.to_string()),
            source_chunk_id: None,
            extracted_by: Some(source.to_string()),
            caused_by: None,
            derived_from: None,
            consolidated_at: None,
            certifiability_class: None,
            object_datatype: None,
        };
        self.inner.add_triple(&triple, self.namespace, self.y_layer)
    }

    /// Add a batch of triples. Returns generated IDs.
    pub fn add_batch(
        &mut self,
        triples: &[(&str, &str, &str, f64, &str)],
    ) -> Result<Vec<String>, StoreError> {
        let ts: Vec<Triple> = triples
            .iter()
            .map(|(s, p, o, conf, src)| Triple {
                subject: s.to_string(),
                predicate: p.to_string(),
                object: o.to_string(),
                graph: None,
                confidence: Some(*conf),
                source_document: Some(src.to_string()),
                source_chunk_id: None,
                extracted_by: Some(src.to_string()),
                caused_by: None,
                derived_from: None,
                consolidated_at: None,
                certifiability_class: None,
                object_datatype: None,
            })
            .collect();
        self.inner.add_batch(&ts, self.namespace, self.y_layer)
    }

    /// Remove a triple by ID. Returns true if found.
    pub fn remove(&mut self, triple_id: &str) -> Result<bool, StoreError> {
        self.inner.delete(triple_id)
    }

    /// Query triples matching pattern. None means wildcard (match all).
    pub fn query(
        &self,
        subject: Option<&str>,
        predicate: Option<&str>,
        object: Option<&str>,
    ) -> Result<Vec<StoredTriple>, StoreError> {
        let spec = QuerySpec {
            subject: subject.map(|s| s.to_string()),
            predicate: predicate.map(|s| s.to_string()),
            object: object.map(|s| s.to_string()),
            namespace: Some(self.namespace),
            ..Default::default()
        };
        let batches = self.inner.query(&spec)?;
        Ok(batches_to_stored_triples(&batches))
    }

    /// Count triples matching pattern.
    pub fn count(
        &self,
        subject: Option<&str>,
        predicate: Option<&str>,
        object: Option<&str>,
    ) -> usize {
        self.query(subject, predicate, object)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Get a single triple by ID.
    pub fn get(&self, triple_id: &str) -> Option<StoredTriple> {
        let spec = QuerySpec {
            include_deleted: false,
            ..Default::default()
        };
        let batches = self.inner.query(&spec).ok()?;
        for batch in &batches {
            let ids = batch
                .column(col::TRIPLE_ID)
                .as_any()
                .downcast_ref::<StringArray>()?;
            for i in 0..ids.len() {
                if ids.value(i) == triple_id {
                    return Some(extract_stored_triple(batch, i));
                }
            }
        }
        None
    }

    /// Update confidence on an existing triple. Returns true if found.
    pub fn update_confidence(
        &mut self,
        triple_id: &str,
        confidence: f64,
    ) -> Result<bool, StoreError> {
        // Find the triple, get its data, remove old, add new with updated confidence
        let existing = match self.get(triple_id) {
            Some(t) => t,
            None => return Ok(false),
        };
        self.inner.delete(triple_id)?;
        let triple = Triple {
            subject: existing.subject,
            predicate: existing.predicate,
            object: existing.object,
            graph: existing.graph,
            confidence: Some(confidence),
            source_document: existing.source.clone(),
            source_chunk_id: None,
            extracted_by: existing.source,
            caused_by: None,
            derived_from: None,
            consolidated_at: None,
            certifiability_class: None,
            object_datatype: None,
        };
        self.inner
            .add_triple(&triple, self.namespace, self.y_layer)?;
        Ok(true)
    }

    /// Group and count triples by a field ("subject", "predicate", or "object").
    pub fn group_by(&self, field: &str) -> Result<HashMap<String, usize>, StoreError> {
        let col_idx = match field {
            "subject" => col::SUBJECT,
            "predicate" => col::PREDICATE,
            "object" => col::OBJECT,
            _ => {
                return Err(StoreError::Arrow(
                    arrow::error::ArrowError::InvalidArgumentError(format!(
                        "invalid group_by field: {field}"
                    )),
                ));
            }
        };
        let spec = QuerySpec {
            namespace: Some(self.namespace),
            ..Default::default()
        };
        let batches = self.inner.query(&spec)?;
        let mut counts: HashMap<String, usize> = HashMap::new();
        for batch in &batches {
            let col_array = batch
                .column(col_idx)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("column must be StringArray");
            for i in 0..col_array.len() {
                *counts.entry(col_array.value(i).to_string()).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }

    /// Get store statistics.
    pub fn stats(&self) -> StoreStats {
        let spec = QuerySpec {
            namespace: Some(self.namespace),
            ..Default::default()
        };
        let batches = self.inner.query(&spec).unwrap_or_default();
        let triples = batches_to_stored_triples(&batches);

        let mut subjects = std::collections::HashSet::new();
        let mut predicates = std::collections::HashSet::new();
        let mut objects = std::collections::HashSet::new();
        let mut by_source: HashMap<String, usize> = HashMap::new();

        for t in &triples {
            subjects.insert(t.subject.clone());
            predicates.insert(t.predicate.clone());
            objects.insert(t.object.clone());
            if let Some(ref src) = t.source {
                *by_source.entry(src.clone()).or_insert(0) += 1;
            }
        }

        StoreStats {
            total_triples: triples.len(),
            unique_subjects: subjects.len(),
            unique_predicates: predicates.len(),
            unique_objects: objects.len(),
            by_source,
        }
    }

    /// Total number of triples.
    pub fn len(&self) -> usize {
        self.count(None, None, None)
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a reference to the underlying ArrowGraphStore.
    pub fn inner(&self) -> &ArrowGraphStore {
        &self.inner
    }

    /// Get a mutable reference to the underlying ArrowGraphStore.
    pub fn inner_mut(&mut self) -> &mut ArrowGraphStore {
        &mut self.inner
    }
}

impl Default for SimpleTripleStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helper functions ──────────────────────────────────────────────────

pub fn extract_stored_triple(batch: &RecordBatch, idx: usize) -> StoredTriple {
    let ids = batch
        .column(col::TRIPLE_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("triple_id column");
    let subjects = batch
        .column(col::SUBJECT)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("subject column");
    let predicates = batch
        .column(col::PREDICATE)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("predicate column");
    let objects = batch
        .column(col::OBJECT)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("object column");
    let graphs = batch
        .column(col::GRAPH)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("graph column");
    let confidences = batch
        .column(col::CONFIDENCE)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("confidence column");
    let sources = batch
        .column(col::EXTRACTED_BY)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("extracted_by column");

    StoredTriple {
        id: ids.value(idx).to_string(),
        subject: subjects.value(idx).to_string(),
        predicate: predicates.value(idx).to_string(),
        object: objects.value(idx).to_string(),
        graph: if graphs.is_null(idx) {
            None
        } else {
            Some(graphs.value(idx).to_string())
        },
        confidence: if confidences.is_null(idx) {
            1.0
        } else {
            confidences.value(idx)
        },
        source: if sources.is_null(idx) {
            None
        } else {
            Some(sources.value(idx).to_string())
        },
    }
}

pub fn batches_to_stored_triples(batches: &[RecordBatch]) -> Vec<StoredTriple> {
    let mut result = Vec::new();
    for batch in batches {
        for i in 0..batch.num_rows() {
            result.push(extract_stored_triple(batch, i));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_query() {
        let mut store = SimpleTripleStore::new();
        let id = store.add("Alice", "knows", "Bob", 0.9, "test").unwrap();
        assert!(!id.is_empty());
        assert_eq!(store.len(), 1);

        let results = store.query(Some("Alice"), None, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].subject, "Alice");
        assert_eq!(results[0].predicate, "knows");
        assert_eq!(results[0].object, "Bob");
        assert!((results[0].confidence - 0.9).abs() < 1e-10);
    }

    #[test]
    fn test_remove() {
        let mut store = SimpleTripleStore::new();
        let id = store.add("s", "p", "o", 1.0, "test").unwrap();
        assert_eq!(store.len(), 1);

        assert!(store.remove(&id).unwrap());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_remove_nonexistent() {
        let mut store = SimpleTripleStore::new();
        assert!(!store.remove("nonexistent").unwrap());
    }

    #[test]
    fn test_query_wildcard() {
        let mut store = SimpleTripleStore::new();
        store.add("Alice", "knows", "Bob", 0.9, "test").unwrap();
        store.add("Alice", "likes", "Carol", 0.8, "test").unwrap();
        store.add("Bob", "knows", "Carol", 0.7, "test").unwrap();

        // Query by subject
        assert_eq!(store.query(Some("Alice"), None, None).unwrap().len(), 2);
        // Query by predicate
        assert_eq!(store.query(None, Some("knows"), None).unwrap().len(), 2);
        // Query by object
        assert_eq!(store.query(None, None, Some("Carol")).unwrap().len(), 2);
        // Query by (s, p)
        assert_eq!(
            store
                .query(Some("Alice"), Some("knows"), None)
                .unwrap()
                .len(),
            1
        );
        // Query all
        assert_eq!(store.query(None, None, None).unwrap().len(), 3);
    }

    #[test]
    fn test_count() {
        let mut store = SimpleTripleStore::new();
        store.add("s1", "p", "o1", 1.0, "test").unwrap();
        store.add("s2", "p", "o2", 1.0, "test").unwrap();
        assert_eq!(store.count(None, None, None), 2);
        assert_eq!(store.count(Some("s1"), None, None), 1);
        assert_eq!(store.count(Some("nonexistent"), None, None), 0);
    }

    #[test]
    fn test_get_by_id() {
        let mut store = SimpleTripleStore::new();
        let id = store.add("s", "p", "o", 0.85, "test").unwrap();

        let triple = store.get(&id).unwrap();
        assert_eq!(triple.id, id);
        assert_eq!(triple.subject, "s");
        assert!((triple.confidence - 0.85).abs() < 1e-10);

        assert!(store.get("nonexistent").is_none());
    }

    #[test]
    fn test_update_confidence() {
        let mut store = SimpleTripleStore::new();
        let id = store.add("s", "p", "o", 0.5, "test").unwrap();

        assert!(store.update_confidence(&id, 0.95).unwrap());
        // Old id is gone (delete+re-add), but the triple exists with new confidence
        let results = store.query(Some("s"), Some("p"), Some("o")).unwrap();
        assert_eq!(results.len(), 1);
        assert!((results[0].confidence - 0.95).abs() < 1e-10);

        assert!(!store.update_confidence("nonexistent", 0.5).unwrap());
    }

    #[test]
    fn test_group_by() {
        let mut store = SimpleTripleStore::new();
        store.add("Alice", "knows", "Bob", 1.0, "test").unwrap();
        store.add("Alice", "likes", "Carol", 1.0, "test").unwrap();
        store.add("Bob", "knows", "Carol", 1.0, "test").unwrap();

        let by_subj = store.group_by("subject").unwrap();
        assert_eq!(by_subj["Alice"], 2);
        assert_eq!(by_subj["Bob"], 1);

        let by_pred = store.group_by("predicate").unwrap();
        assert_eq!(by_pred["knows"], 2);
        assert_eq!(by_pred["likes"], 1);
    }

    #[test]
    fn test_stats() {
        let mut store = SimpleTripleStore::new();
        store.add("s1", "p1", "o1", 1.0, "src_a").unwrap();
        store.add("s2", "p1", "o2", 1.0, "src_a").unwrap();
        store.add("s1", "p2", "o1", 1.0, "src_b").unwrap();

        let stats = store.stats();
        assert_eq!(stats.total_triples, 3);
        assert_eq!(stats.unique_subjects, 2);
        assert_eq!(stats.unique_predicates, 2);
        assert_eq!(stats.unique_objects, 2);
        assert_eq!(stats.by_source["src_a"], 2);
        assert_eq!(stats.by_source["src_b"], 1);
    }

    #[test]
    fn test_batch_add() {
        let mut store = SimpleTripleStore::new();
        let ids = store
            .add_batch(&[
                ("s1", "p", "o1", 0.9, "batch"),
                ("s2", "p", "o2", 0.8, "batch"),
                ("s3", "p", "o3", 0.7, "batch"),
            ])
            .unwrap();
        assert_eq!(ids.len(), 3);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn test_empty_store() {
        let store = SimpleTripleStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert_eq!(store.count(None, None, None), 0);
        assert!(store.query(None, None, None).unwrap().is_empty());
    }

    #[test]
    fn test_batch_performance() {
        let mut store = SimpleTripleStore::new();
        let triples: Vec<(&str, &str, &str, f64, &str)> = (0..1000)
            .map(|_| ("subject", "predicate", "object", 1.0, "perf"))
            .collect();

        let start = std::time::Instant::now();
        store.add_batch(&triples).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(store.len(), 1000);
        assert!(
            elapsed.as_millis() < 50,
            "1000 triple batch add took {:?}",
            elapsed
        );
    }
}
