//! Shared builder for `triples_schema()` RecordBatches (CH-4712).
//!
//! Every triples-shaped `RecordBatch` in the workspace MUST be built through
//! this module — production write paths and test fixtures alike. Hand-rolled
//! column lists are the failure class this kills: column count is a *runtime*
//! error, so a builder that falls behind the schema slips through crate-scoped
//! tests and surfaces as red `cargo test --workspace` tails (EX-4681 →
//! PROP-2686/PROP-2689; EX-4682 left four crates hidden-red). When a column is
//! added to [`triples_schema`](crate::schema::triples_schema), extend
//! [`TriplesBatchBuilder::build`] here — call sites do not change.

use crate::namespace::Namespace;
use crate::schema::triples_schema;
use crate::store::Triple;
use crate::y_layer::YLayer;

use arrow::array::{
    BooleanArray, Float64Array, RecordBatch, StringArray, TimestampMillisecondArray, UInt8Array,
};
use arrow::error::ArrowError;
use std::sync::Arc;

/// Build a `triples_schema()` batch with fresh UUID triple IDs, `created_at = now`,
/// `deleted = false`. Returns the generated IDs alongside the batch.
///
/// This is the one-call form that fits most production write paths. Sites that
/// need explicit IDs, fixed timestamps, or deleted rows (snapshots, test
/// fixtures) use [`TriplesBatchBuilder`] directly.
pub fn triples_record_batch(
    triples: &[Triple],
    namespace: Namespace,
    y_layer: YLayer,
) -> Result<(Vec<String>, RecordBatch), ArrowError> {
    TriplesBatchBuilder::new(triples, namespace, y_layer).build()
}

enum CreatedAt<'a> {
    Now,
    Constant(i64),
    PerRow(&'a [i64]),
}

/// Configurable builder over [`triples_record_batch`] for sites that control
/// triple IDs, timestamps, or logical-delete flags.
///
/// Defaults match the production write path: fresh UUID v4 IDs, `created_at`
/// = now, all rows live. `epistemic_status` is always null at construction —
/// it is set only by governed write-back (EX-4682), so there is deliberately
/// no knob for it.
pub struct TriplesBatchBuilder<'a> {
    triples: &'a [Triple],
    namespace: Namespace,
    y_layer: YLayer,
    ids: Option<&'a [String]>,
    created_at: CreatedAt<'a>,
    deleted: Option<&'a [bool]>,
    salience: Option<&'a [f64]>,
}

impl<'a> TriplesBatchBuilder<'a> {
    pub fn new(triples: &'a [Triple], namespace: Namespace, y_layer: YLayer) -> Self {
        TriplesBatchBuilder {
            triples,
            namespace,
            y_layer,
            ids: None,
            created_at: CreatedAt::Now,
            deleted: None,
            salience: None,
        }
    }

    /// Use explicit triple IDs instead of fresh UUIDs (snapshots, fixtures).
    pub fn with_ids(mut self, ids: &'a [String]) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Set every row's `created_at` to a fixed timestamp (ms since epoch).
    pub fn with_created_at_ms(mut self, ms: i64) -> Self {
        self.created_at = CreatedAt::Constant(ms);
        self
    }

    /// Set `created_at` per row (ms since epoch).
    pub fn with_created_at_per_row(mut self, ms: &'a [i64]) -> Self {
        self.created_at = CreatedAt::PerRow(ms);
        self
    }

    /// Set the logical-delete flag per row (default: all `false`).
    pub fn with_deleted(mut self, deleted: &'a [bool]) -> Self {
        self.deleted = Some(deleted);
        self
    }

    /// Set the salience/importance score per row (EX-5021; default: all null/unscored).
    /// This is the **only** way salience enters a batch — it is set by the governed dream
    /// write-back (the ImportanceScorer), never at ordinary construction. Salience ranks
    /// retrieval; it never affects `epistemic_status` or the Proven slice.
    pub fn with_salience(mut self, salience: &'a [f64]) -> Self {
        self.salience = Some(salience);
        self
    }

    /// Build the batch. Returns `(triple_ids, batch)`; IDs are the explicit
    /// ones when [`with_ids`](Self::with_ids) was used, otherwise fresh UUIDs.
    pub fn build(self) -> Result<(Vec<String>, RecordBatch), ArrowError> {
        let n = self.triples.len();
        let schema = Arc::new(triples_schema());

        if n == 0 {
            return Ok((vec![], RecordBatch::new_empty(schema)));
        }

        let ids: Vec<String> = match self.ids {
            Some(ids) => {
                if ids.len() != n {
                    return Err(ArrowError::InvalidArgumentError(format!(
                        "TriplesBatchBuilder: {} ids for {} triples",
                        ids.len(),
                        n
                    )));
                }
                ids.to_vec()
            }
            None => (0..n).map(|_| uuid::Uuid::new_v4().to_string()).collect(),
        };

        let timestamps: Vec<i64> = match self.created_at {
            CreatedAt::Now => vec![chrono::Utc::now().timestamp_millis(); n],
            CreatedAt::Constant(ms) => vec![ms; n],
            CreatedAt::PerRow(ms) => {
                if ms.len() != n {
                    return Err(ArrowError::InvalidArgumentError(format!(
                        "TriplesBatchBuilder: {} created_at values for {} triples",
                        ms.len(),
                        n
                    )));
                }
                ms.to_vec()
            }
        };

        let deleted: Vec<bool> = match self.deleted {
            Some(d) => {
                if d.len() != n {
                    return Err(ArrowError::InvalidArgumentError(format!(
                        "TriplesBatchBuilder: {} deleted flags for {} triples",
                        d.len(),
                        n
                    )));
                }
                d.to_vec()
            }
            None => vec![false; n],
        };

        // EX-5021: salience is null at ordinary construction; the governed dream write-back
        // supplies a per-row score via `with_salience`.
        let salience: Vec<Option<f64>> = match self.salience {
            Some(s) => {
                if s.len() != n {
                    return Err(ArrowError::InvalidArgumentError(format!(
                        "TriplesBatchBuilder: {} salience values for {} triples",
                        s.len(),
                        n
                    )));
                }
                s.iter().map(|&v| Some(v)).collect()
            }
            None => vec![None; n],
        };

        let t = self.triples;
        let id_strs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        let subjects: Vec<&str> = t.iter().map(|t| t.subject.as_str()).collect();
        let predicates: Vec<&str> = t.iter().map(|t| t.predicate.as_str()).collect();
        let objects: Vec<&str> = t.iter().map(|t| t.object.as_str()).collect();
        let graphs: Vec<Option<&str>> = t.iter().map(|t| t.graph.as_deref()).collect();
        let ns_vals: Vec<&str> = vec![self.namespace.as_str(); n];
        let layer_vals: Vec<u8> = vec![self.y_layer.as_u8(); n];
        let confidences: Vec<Option<f64>> = t.iter().map(|t| t.confidence).collect();
        let source_docs: Vec<Option<&str>> =
            t.iter().map(|t| t.source_document.as_deref()).collect();
        let source_chunks: Vec<Option<&str>> =
            t.iter().map(|t| t.source_chunk_id.as_deref()).collect();
        let extracted: Vec<Option<&str>> = t.iter().map(|t| t.extracted_by.as_deref()).collect();
        let caused_by: Vec<Option<&str>> = t.iter().map(|t| t.caused_by.as_deref()).collect();
        let derived_from: Vec<Option<&str>> = t.iter().map(|t| t.derived_from.as_deref()).collect();
        let consolidated_at: Vec<Option<i64>> = t.iter().map(|t| t.consolidated_at).collect();
        let certifiability_class: Vec<Option<&str>> = t
            .iter()
            .map(|t| t.certifiability_class.as_deref())
            .collect();
        // EX-4681: XSD datatype sidecar (None = plain string literal).
        let object_datatype: Vec<Option<&str>> =
            t.iter().map(|t| t.object_datatype.as_deref()).collect();

        let batch = RecordBatch::try_new(
            schema,
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
                Arc::new(StringArray::from(vec![None::<&str>; n])),
                // EX-5021: salience — null (= unscored) unless the governed dream write-back
                // supplied scores via `with_salience`.
                Arc::new(Float64Array::from(salience)),
            ],
        )?;

        Ok((ids, batch))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::col;
    use arrow::array::Array;

    fn sample_triples(n: usize) -> Vec<Triple> {
        (0..n)
            .map(|i| Triple {
                subject: format!("s{i}"),
                predicate: "rdf:type".to_string(),
                object: "Entity".to_string(),
                confidence: Some(0.9),
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn batch_matches_canonical_schema() {
        let (ids, batch) =
            triples_record_batch(&sample_triples(3), Namespace::World, YLayer::Semantic)
                .expect("build");
        assert_eq!(batch.schema().as_ref(), &triples_schema());
        assert_eq!(batch.num_rows(), 3);
        assert_eq!(ids.len(), 3);
        // Generated IDs are written to the triple_id column.
        let id_col = batch
            .column(col::TRIPLE_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(id_col.value(0), ids[0]);
    }

    #[test]
    fn empty_input_yields_empty_batch_with_schema() {
        let (ids, batch) =
            triples_record_batch(&[], Namespace::World, YLayer::Semantic).expect("build empty");
        assert!(ids.is_empty());
        assert_eq!(batch.num_rows(), 0);
        assert_eq!(batch.schema().as_ref(), &triples_schema());
    }

    #[test]
    fn explicit_ids_timestamps_and_deleted_are_used() {
        let triples = sample_triples(2);
        let ids = vec!["t-0".to_string(), "t-1".to_string()];
        let ts = [100i64, 200i64];
        let deleted = [false, true];
        let (out_ids, batch) =
            TriplesBatchBuilder::new(&triples, Namespace::Self_, YLayer::Experience)
                .with_ids(&ids)
                .with_created_at_per_row(&ts)
                .with_deleted(&deleted)
                .build()
                .expect("build");
        assert_eq!(out_ids, ids);

        let id_col = batch
            .column(col::TRIPLE_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(id_col.value(1), "t-1");

        let ts_col = batch
            .column(col::CREATED_AT)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert_eq!(ts_col.value(0), 100);
        assert_eq!(ts_col.value(1), 200);

        let del_col = batch
            .column(col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        assert!(!del_col.value(0));
        assert!(del_col.value(1));

        let ns_col = batch
            .column(col::NAMESPACE)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(ns_col.value(0), Namespace::Self_.as_str());
    }

    #[test]
    fn length_mismatches_are_rejected() {
        let triples = sample_triples(2);
        let ids = vec!["only-one".to_string()];
        assert!(
            TriplesBatchBuilder::new(&triples, Namespace::World, YLayer::Semantic)
                .with_ids(&ids)
                .build()
                .is_err()
        );
        assert!(
            TriplesBatchBuilder::new(&triples, Namespace::World, YLayer::Semantic)
                .with_created_at_per_row(&[1])
                .build()
                .is_err()
        );
        assert!(
            TriplesBatchBuilder::new(&triples, Namespace::World, YLayer::Semantic)
                .with_deleted(&[true])
                .build()
                .is_err()
        );
    }

    #[test]
    fn epistemic_status_is_null_at_construction() {
        let (_, batch) =
            triples_record_batch(&sample_triples(1), Namespace::World, YLayer::Semantic)
                .expect("build");
        let ep = batch
            .column(col::EPISTEMIC_STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(ep.is_null(0));
    }
}
