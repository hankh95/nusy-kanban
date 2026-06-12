//! ArrowGraphStore — the core partitioned graph store.
//!
//! Holds triples partitioned by namespace, with Y-layer as a column filter.
//! Supports add, query, delete (logical), and batch operations.

use crate::namespace::Namespace;
use crate::schema::{col, triples_schema};
use crate::y_layer::YLayer;

use arrow::array::{
    Array, BooleanArray, Float64Array, RecordBatch, StringArray, TimestampMillisecondArray,
    UInt8Array,
};
use arrow::compute;
use arrow::datatypes::SchemaRef;
use std::collections::HashMap;
use std::sync::Arc;

/// Errors from store operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Unknown namespace: {0}")]
    UnknownNamespace(String),

    #[error("Invalid Y-layer: {0}")]
    InvalidYLayer(u8),

    #[error("Triple not found: {0}")]
    TripleNotFound(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// A single triple to be added to the store.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub graph: Option<String>,
    pub confidence: Option<f64>,
    pub source_document: Option<String>,
    /// FK to ChunkTable for fine-grained Y0 provenance.
    pub source_chunk_id: Option<String>,
    pub extracted_by: Option<String>,
    /// The triple_id of the triple that caused this one (causal chain).
    pub caused_by: Option<String>,
    /// The triple_id of the triple this was derived from.
    pub derived_from: Option<String>,
    /// Timestamp (ms since epoch) when this triple was consolidated.
    pub consolidated_at: Option<i64>,
    /// Certifiability class: "symbolic" (graph-backed, provable),
    /// "neural" (LLM-generated, probabilistic), "co-voted" (both agreed).
    /// EX-3570: Tags triples for PAR tracking and routing decisions.
    pub certifiability_class: Option<String>,
    /// EX-4681: XSD datatype URI of `object` (None = plain string literal).
    pub object_datatype: Option<String>,
}

/// Filter specification for queries.
#[derive(Debug, Default, Clone)]
pub struct QuerySpec {
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub object: Option<String>,
    pub namespace: Option<Namespace>,
    pub y_layer: Option<YLayer>,
    /// Named-graph filter (EX-4680): when set, only triples whose `graph` column
    /// equals this value match. Rows with a NULL graph never match a named-graph
    /// query. The handle form is `canonical_url|version`
    /// (see [`KnowledgeArtifact::named_graph`](crate::KnowledgeArtifact::named_graph)).
    pub graph: Option<String>,
    pub include_deleted: bool,
}

/// A node in a causal derivation chain.
#[derive(Debug, Clone, PartialEq)]
pub struct CausalNode {
    pub triple_id: String,
    pub caused_by: Option<String>,
    pub derived_from: Option<String>,
}

/// The core Arrow-native graph store, partitioned by namespace.
///
/// Each namespace holds a vector of RecordBatches (appended over time).
/// Queries filter by namespace first, then by column predicates.
pub struct ArrowGraphStore {
    schema: SchemaRef,
    /// Per-namespace storage: Vec<RecordBatch> (append-only within a partition).
    partitions: HashMap<Namespace, Vec<RecordBatch>>,
}

impl ArrowGraphStore {
    /// Create a new empty store.
    pub fn new() -> Self {
        let schema = Arc::new(triples_schema());
        let mut partitions = HashMap::new();
        for ns in Namespace::ALL {
            partitions.insert(ns, Vec::new());
        }
        ArrowGraphStore { schema, partitions }
    }

    /// Get the triples schema.
    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// Add a single triple to the specified namespace and Y-layer.
    pub fn add_triple(
        &mut self,
        triple: &Triple,
        namespace: Namespace,
        y_layer: YLayer,
    ) -> Result<String> {
        self.add_batch(std::slice::from_ref(triple), namespace, y_layer)
            .map(|ids| ids.into_iter().next().unwrap())
    }

    /// Add a batch of triples to the specified namespace and Y-layer.
    /// Returns the generated triple IDs.
    pub fn add_batch(
        &mut self,
        triples: &[Triple],
        namespace: Namespace,
        y_layer: YLayer,
    ) -> Result<Vec<String>> {
        let n = triples.len();
        if n == 0 {
            return Ok(vec![]);
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        let ns_str = namespace.as_str();
        let layer_val = y_layer.as_u8();

        let ids: Vec<String> = (0..n).map(|_| uuid::Uuid::new_v4().to_string()).collect();

        let subjects: Vec<&str> = triples.iter().map(|t| t.subject.as_str()).collect();
        let predicates: Vec<&str> = triples.iter().map(|t| t.predicate.as_str()).collect();
        let objects: Vec<&str> = triples.iter().map(|t| t.object.as_str()).collect();
        let graphs: Vec<Option<&str>> = triples.iter().map(|t| t.graph.as_deref()).collect();
        let ns_vals: Vec<&str> = vec![ns_str; n];
        let layer_vals: Vec<u8> = vec![layer_val; n];
        let confidences: Vec<Option<f64>> = triples.iter().map(|t| t.confidence).collect();
        let source_docs: Vec<Option<&str>> = triples
            .iter()
            .map(|t| t.source_document.as_deref())
            .collect();
        let source_chunks: Vec<Option<&str>> = triples
            .iter()
            .map(|t| t.source_chunk_id.as_deref())
            .collect();
        let extracted: Vec<Option<&str>> =
            triples.iter().map(|t| t.extracted_by.as_deref()).collect();
        let caused_by: Vec<Option<&str>> = triples.iter().map(|t| t.caused_by.as_deref()).collect();
        let derived_from: Vec<Option<&str>> =
            triples.iter().map(|t| t.derived_from.as_deref()).collect();
        let consolidated_at: Vec<Option<i64>> = triples.iter().map(|t| t.consolidated_at).collect();
        let timestamps: Vec<i64> = vec![now_ms; n];
        let deleted: Vec<bool> = vec![false; n];
        let certifiability_class: Vec<Option<&str>> = triples
            .iter()
            .map(|t| t.certifiability_class.as_deref())
            .collect();
        // EX-4681: XSD datatype sidecar (None = plain string literal).
        let object_datatype: Vec<Option<&str>> = triples
            .iter()
            .map(|t| t.object_datatype.as_deref())
            .collect();
        let id_strs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();

        let batch = RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(StringArray::from(id_strs)),
                Arc::new(StringArray::from(subjects)),
                Arc::new(StringArray::from(predicates)),
                Arc::new(StringArray::from(objects)),
                Arc::new(StringArray::from(graphs)),
                Arc::new(StringArray::from(ns_vals)),
                Arc::new(UInt8Array::from(layer_vals)),
                Arc::new(Float64Array::from(confidences)),
                Arc::new(StringArray::from(source_docs)),
                Arc::new(StringArray::from(source_chunks)),
                Arc::new(StringArray::from(extracted)),
                Arc::new(TimestampMillisecondArray::from(timestamps).with_timezone("UTC")),
                Arc::new(StringArray::from(caused_by)),
                Arc::new(StringArray::from(derived_from)),
                Arc::new(TimestampMillisecondArray::from(consolidated_at).with_timezone("UTC")),
                Arc::new(BooleanArray::from(deleted)),
                Arc::new(StringArray::from(certifiability_class)),
                Arc::new(StringArray::from(object_datatype)),
                // EX-4682: epistemic_status — null (= asserted) at construction; set to
                // derived/believed/retracted only by governed write-back.
                Arc::new(StringArray::from(vec![None::<&str>; triples.len()])),
            ],
        )?;

        self.partitions.get_mut(&namespace).unwrap().push(batch);

        Ok(ids)
    }

    /// Query triples matching the given spec.
    pub fn query(&self, spec: &QuerySpec) -> Result<Vec<RecordBatch>> {
        let namespaces: Vec<Namespace> = match spec.namespace {
            Some(ns) => vec![ns],
            None => Namespace::ALL.to_vec(),
        };

        let mut results = Vec::new();

        for ns in namespaces {
            let batches = self.partitions.get(&ns).unwrap();
            for batch in batches {
                let filtered = self.filter_batch(batch, spec)?;
                if filtered.num_rows() > 0 {
                    results.push(filtered);
                }
            }
        }

        Ok(results)
    }

    /// Total number of non-deleted triples across all namespaces.
    pub fn len(&self) -> usize {
        let spec = QuerySpec::default();
        self.query(&spec)
            .unwrap_or_default()
            .iter()
            .map(|b| b.num_rows())
            .sum()
    }

    /// Whether the store has no non-deleted triples.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total number of triples including deleted.
    pub fn len_all(&self) -> usize {
        self.partitions
            .values()
            .flat_map(|batches| batches.iter())
            .map(|b| b.num_rows())
            .sum()
    }

    /// Logically delete a triple by ID (sets deleted=true).
    pub fn delete(&mut self, triple_id: &str) -> Result<bool> {
        for batches in self.partitions.values_mut() {
            for batch in batches.iter_mut() {
                let id_col = batch
                    .column(col::TRIPLE_ID)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("triple_id column must be StringArray");

                let mut found_idx = None;
                for i in 0..id_col.len() {
                    if id_col.value(i) == triple_id {
                        found_idx = Some(i);
                        break;
                    }
                }

                if let Some(idx) = found_idx {
                    // Rebuild the batch with the deleted flag set
                    let del_col = batch
                        .column(col::DELETED)
                        .as_any()
                        .downcast_ref::<BooleanArray>()
                        .expect("deleted column must be BooleanArray");

                    let mut new_del: Vec<bool> =
                        (0..del_col.len()).map(|i| del_col.value(i)).collect();
                    new_del[idx] = true;

                    let mut columns: Vec<Arc<dyn Array>> = Vec::new();
                    for c in 0..batch.num_columns() {
                        if c == col::DELETED {
                            columns.push(Arc::new(BooleanArray::from(new_del.clone())));
                        } else {
                            columns.push(batch.column(c).clone());
                        }
                    }

                    *batch = RecordBatch::try_new(self.schema.clone(), columns)?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Get all RecordBatches for a given namespace (including deleted triples).
    pub fn get_namespace_batches(&self, namespace: Namespace) -> &[RecordBatch] {
        self.partitions
            .get(&namespace)
            .map_or(&[], |v| v.as_slice())
    }

    /// Replace all data for a namespace (used by checkout/restore).
    pub fn set_namespace_batches(&mut self, namespace: Namespace, batches: Vec<RecordBatch>) {
        self.partitions.insert(namespace, batches);
    }

    /// Follow the caused_by/derived_from chain from a triple to build a derivation graph.
    ///
    /// Returns a list of (triple_id, caused_by, derived_from) tuples representing
    /// the full causal ancestry of the given triple. The first element is always the
    /// queried triple itself. Traversal is breadth-first, following both `caused_by`
    /// and `derived_from` links.
    pub fn causal_chain(&self, triple_id: &str) -> Vec<CausalNode> {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(triple_id.to_string());

        // Build an index of triple_id → (caused_by, derived_from) for efficient lookup
        let mut index: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        for batches in self.partitions.values() {
            for batch in batches {
                let id_col = batch
                    .column(col::TRIPLE_ID)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("triple_id column");
                let caused_col = batch
                    .column(col::CAUSED_BY)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("caused_by column");
                let derived_col = batch
                    .column(col::DERIVED_FROM)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("derived_from column");
                let del_col = batch
                    .column(col::DELETED)
                    .as_any()
                    .downcast_ref::<BooleanArray>()
                    .expect("deleted column");

                for i in 0..batch.num_rows() {
                    if del_col.value(i) {
                        continue;
                    }
                    let id = id_col.value(i).to_string();
                    let caused = if caused_col.is_null(i) {
                        None
                    } else {
                        Some(caused_col.value(i).to_string())
                    };
                    let derived = if derived_col.is_null(i) {
                        None
                    } else {
                        Some(derived_col.value(i).to_string())
                    };
                    index.insert(id, (caused, derived));
                }
            }
        }

        while let Some(tid) = queue.pop_front() {
            if !visited.insert(tid.clone()) {
                continue;
            }
            if let Some((caused, derived)) = index.get(&tid) {
                result.push(CausalNode {
                    triple_id: tid.clone(),
                    caused_by: caused.clone(),
                    derived_from: derived.clone(),
                });
                if let Some(cb) = caused
                    && !visited.contains(cb)
                {
                    queue.push_back(cb.clone());
                }
                if let Some(df) = derived
                    && !visited.contains(df)
                {
                    queue.push_back(df.clone());
                }
            }
        }

        result
    }

    /// Clear all data from the store.
    pub fn clear(&mut self) {
        for batches in self.partitions.values_mut() {
            batches.clear();
        }
    }

    /// Filter a RecordBatch by the QuerySpec predicates.
    fn filter_batch(&self, batch: &RecordBatch, spec: &QuerySpec) -> Result<RecordBatch> {
        let n = batch.num_rows();
        let mut mask = BooleanArray::from(vec![true; n]);

        // Filter out deleted unless include_deleted
        if !spec.include_deleted {
            let del_col = batch
                .column(col::DELETED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("deleted column must be BooleanArray");
            let not_deleted = compute::not(del_col)?;
            mask = compute::and(&mask, &not_deleted)?;
        }

        // Filter by subject
        if let Some(ref subj) = spec.subject {
            let c = batch
                .column(col::SUBJECT)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("subject column must be StringArray");
            let eq = string_eq_scalar(c, subj);
            mask = compute::and(&mask, &eq)?;
        }

        // Filter by predicate
        if let Some(ref pred) = spec.predicate {
            let c = batch
                .column(col::PREDICATE)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("predicate column must be StringArray");
            let eq = string_eq_scalar(c, pred);
            mask = compute::and(&mask, &eq)?;
        }

        // Filter by object
        if let Some(ref obj) = spec.object {
            let c = batch
                .column(col::OBJECT)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("object column must be StringArray");
            let eq = string_eq_scalar(c, obj);
            mask = compute::and(&mask, &eq)?;
        }

        // Filter by named graph (EX-4680): equality against the nullable `graph`
        // column. NULL-graph rows produce a null mask entry → excluded by filter.
        if let Some(ref g) = spec.graph {
            let c = batch
                .column(col::GRAPH)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("graph column must be StringArray");
            let eq = string_eq_scalar(c, g);
            mask = compute::and(&mask, &eq)?;
        }

        // Filter by Y-layer
        if let Some(layer) = spec.y_layer {
            let c = batch
                .column(col::Y_LAYER)
                .as_any()
                .downcast_ref::<UInt8Array>()
                .expect("y_layer column must be UInt8Array");
            let eq = u8_eq_scalar(c, layer.as_u8());
            mask = compute::and(&mask, &eq)?;
        }

        let filtered = compute::filter_record_batch(batch, &mask)?;
        Ok(filtered)
    }
}

impl Default for ArrowGraphStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Scalar string equality: returns BooleanArray where each element == value.
fn string_eq_scalar(array: &StringArray, value: &str) -> BooleanArray {
    let bools: Vec<bool> = (0..array.len()).map(|i| array.value(i) == value).collect();
    BooleanArray::from(bools)
}

/// Scalar u8 equality.
fn u8_eq_scalar(array: &UInt8Array, value: u8) -> BooleanArray {
    let bools: Vec<bool> = (0..array.len()).map(|i| array.value(i) == value).collect();
    BooleanArray::from(bools)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_triple(subj: &str, pred: &str, obj: &str) -> Triple {
        Triple {
            subject: subj.to_string(),
            predicate: pred.to_string(),
            object: obj.to_string(),
            graph: None,
            confidence: Some(0.9),
            source_document: None,
            source_chunk_id: None,
            extracted_by: Some("test".to_string()),
            caused_by: None,
            derived_from: None,
            consolidated_at: None,
            certifiability_class: None,
            object_datatype: None,
        }
    }

    #[test]
    fn test_add_and_query_single() {
        let mut store = ArrowGraphStore::new();
        let id = store
            .add_triple(
                &sample_triple("s1", "p1", "o1"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();

        assert!(!id.is_empty());
        assert_eq!(store.len(), 1);

        let results = store
            .query(&QuerySpec {
                subject: Some("s1".to_string()),
                ..Default::default()
            })
            .unwrap();
        let total: usize = results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn test_named_graph_filter_round_trip() {
        // EX-4680 Phase 2/5: triples tagged with an artifact's named-graph handle
        // are retrievable by that handle; NULL-graph and other-graph triples are not.
        use crate::artifacts::{KnowledgeArtifact, Version};
        let artifact = KnowledgeArtifact {
            artifact_id: "rules-a".to_string(),
            artifact_type: "rule-set".to_string(),
            version: Version::new(1, 2, 0),
            status: crate::artifacts::ArtifactStatus::Active,
            canonical_url: "https://nusy.dev/ka/rules-a".to_string(),
            steward: "Air".to_string(),
            date: 1_700_000_000_000,
            effective_start: None,
            effective_end: None,
            supersedes: None,
        };
        let handle = artifact.named_graph(); // "https://nusy.dev/ka/rules-a|1.2.0"

        let mut store = ArrowGraphStore::new();
        let mut in_graph = sample_triple("s_in", "p", "o");
        in_graph.graph = Some(handle.clone());
        let mut other_graph = sample_triple("s_other", "p", "o");
        other_graph.graph = Some("https://nusy.dev/ka/rules-a|1.0.0".to_string());
        let null_graph = sample_triple("s_null", "p", "o"); // graph: None

        store
            .add_batch(
                &[in_graph, other_graph, null_graph],
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();

        let results = store
            .query(&QuerySpec {
                graph: Some(handle.clone()),
                ..Default::default()
            })
            .unwrap();
        let rows: Vec<RecordBatch> = results;
        let total: usize = rows.iter().map(|b| b.num_rows()).sum();
        assert_eq!(
            total, 1,
            "only the triple tagged with the exact handle matches"
        );
        // Confirm it is the right subject.
        let subj = rows[0]
            .column(col::SUBJECT)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(subj.value(0), "s_in");
    }

    #[test]
    fn test_namespace_isolation() {
        let mut store = ArrowGraphStore::new();

        // Add 100 triples to world
        let world_triples: Vec<Triple> = (0..100)
            .map(|i| sample_triple(&format!("w{i}"), "rdf:type", "Thing"))
            .collect();
        store
            .add_batch(&world_triples, Namespace::World, YLayer::Semantic)
            .unwrap();

        // Add 100 triples to work
        let work_triples: Vec<Triple> = (0..100)
            .map(|i| sample_triple(&format!("k{i}"), "rdf:type", "Task"))
            .collect();
        store
            .add_batch(&work_triples, Namespace::Work, YLayer::Semantic)
            .unwrap();

        // Query world — should return exactly 100
        let world_results = store
            .query(&QuerySpec {
                namespace: Some(Namespace::World),
                ..Default::default()
            })
            .unwrap();
        let world_count: usize = world_results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(world_count, 100);

        // Query work — should return exactly 100
        let work_results = store
            .query(&QuerySpec {
                namespace: Some(Namespace::Work),
                ..Default::default()
            })
            .unwrap();
        let work_count: usize = work_results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(work_count, 100);

        // Total
        assert_eq!(store.len(), 200);
    }

    #[test]
    fn test_ylayer_query() {
        let mut store = ArrowGraphStore::new();

        store
            .add_triple(
                &sample_triple("s1", "p1", "o1"),
                Namespace::World,
                YLayer::Prose,
            )
            .unwrap();
        store
            .add_triple(
                &sample_triple("s2", "p2", "o2"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();

        let y0_results = store
            .query(&QuerySpec {
                y_layer: Some(YLayer::Prose),
                ..Default::default()
            })
            .unwrap();
        let y0_count: usize = y0_results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(y0_count, 1);
    }

    #[test]
    fn test_logical_delete() {
        let mut store = ArrowGraphStore::new();
        let id = store
            .add_triple(
                &sample_triple("s1", "p1", "o1"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();

        assert_eq!(store.len(), 1);
        assert!(store.delete(&id).unwrap());
        assert_eq!(store.len(), 0);
        assert_eq!(store.len_all(), 1); // Still physically present
    }

    #[test]
    fn test_batch_add_performance() {
        let mut store = ArrowGraphStore::new();

        let triples: Vec<Triple> = (0..10_000)
            .map(|i| sample_triple(&format!("s{i}"), "rdf:type", "Entity"))
            .collect();

        let start = std::time::Instant::now();
        store
            .add_batch(&triples, Namespace::World, YLayer::Semantic)
            .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(store.len(), 10_000);
        // Should be well under 10ms for batch add
        assert!(
            elapsed.as_millis() < 100,
            "Batch add took too long: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_causal_chain_linear() {
        let mut store = ArrowGraphStore::new();

        // Create a chain: t0 → t1 → t2 (each caused_by the previous)
        let t0 = Triple {
            subject: "s0".to_string(),
            predicate: "p".to_string(),
            object: "o0".to_string(),
            caused_by: None,
            derived_from: None,
            ..sample_triple("s0", "p", "o0")
        };
        let id0 = store
            .add_triple(&t0, Namespace::World, YLayer::Semantic)
            .unwrap();

        let t1 = Triple {
            subject: "s1".to_string(),
            predicate: "p".to_string(),
            object: "o1".to_string(),
            caused_by: Some(id0.clone()),
            derived_from: None,
            ..sample_triple("s1", "p", "o1")
        };
        let id1 = store
            .add_triple(&t1, Namespace::World, YLayer::Semantic)
            .unwrap();

        let t2 = Triple {
            subject: "s2".to_string(),
            predicate: "p".to_string(),
            object: "o2".to_string(),
            caused_by: Some(id1.clone()),
            derived_from: None,
            ..sample_triple("s2", "p", "o2")
        };
        let id2 = store
            .add_triple(&t2, Namespace::World, YLayer::Semantic)
            .unwrap();

        // Causal chain from t2 should traverse t2 → t1 → t0
        let chain = store.causal_chain(&id2);
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].triple_id, id2);
        assert_eq!(chain[0].caused_by, Some(id1.clone()));
        assert_eq!(chain[1].triple_id, id1);
        assert_eq!(chain[1].caused_by, Some(id0.clone()));
        assert_eq!(chain[2].triple_id, id0);
        assert_eq!(chain[2].caused_by, None);
    }

    #[test]
    fn test_causal_chain_with_derived_from() {
        let mut store = ArrowGraphStore::new();

        let t0 = Triple {
            subject: "base".to_string(),
            predicate: "p".to_string(),
            object: "original".to_string(),
            caused_by: None,
            derived_from: None,
            ..sample_triple("base", "p", "original")
        };
        let id0 = store
            .add_triple(&t0, Namespace::World, YLayer::Reasoning)
            .unwrap();

        let t1 = Triple {
            subject: "derived".to_string(),
            predicate: "p".to_string(),
            object: "derived_val".to_string(),
            caused_by: None,
            derived_from: Some(id0.clone()),
            ..sample_triple("derived", "p", "derived_val")
        };
        let id1 = store
            .add_triple(&t1, Namespace::World, YLayer::Reasoning)
            .unwrap();

        let chain = store.causal_chain(&id1);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].derived_from, Some(id0.clone()));
        assert_eq!(chain[1].triple_id, id0);
    }

    #[test]
    fn test_causal_chain_nonexistent_triple() {
        let store = ArrowGraphStore::new();
        let chain = store.causal_chain("nonexistent");
        assert!(chain.is_empty());
    }
}
