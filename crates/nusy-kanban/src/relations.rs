//! Relations — cross-item and cross-board links.
//!
//! Supports predicates: implements, spawns, blocks, related_to.

use crate::schema::{rel_col, relations_schema};
use arrow::array::{Array, BooleanArray, RecordBatch, StringArray, TimestampMillisecondArray};
use std::sync::Arc;

/// Errors from relation operations.
#[derive(Debug, thiserror::Error)]
pub enum RelationError {
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Relation not found: {0} → {1} ({2})")]
    NotFound(String, String, String),
}

pub type Result<T> = std::result::Result<T, RelationError>;

/// The relations store — holds relation RecordBatches.
pub struct RelationsStore {
    batches: Vec<RecordBatch>,
    schema: Arc<arrow::datatypes::Schema>,
}

impl RelationsStore {
    pub fn new() -> Self {
        RelationsStore {
            batches: Vec::new(),
            schema: relations_schema(),
        }
    }

    /// Load pre-built relation batches (e.g., from migration or Parquet).
    pub fn load(&mut self, batches: Vec<RecordBatch>) {
        self.batches.extend(batches);
    }

    /// Add a relation between two items.
    pub fn add_relation(
        &mut self,
        source_id: &str,
        target_id: &str,
        predicate: &str,
    ) -> Result<String> {
        let rel_id = uuid::Uuid::new_v4().to_string();
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![rel_id.as_str()])),
                Arc::new(StringArray::from(vec![source_id])),
                Arc::new(StringArray::from(vec![target_id])),
                Arc::new(StringArray::from(vec![predicate])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(BooleanArray::from(vec![false])),
            ],
        )?;

        self.batches.push(batch);
        Ok(rel_id)
    }

    /// Remove a relation (logical delete).
    pub fn remove_relation(
        &mut self,
        source_id: &str,
        target_id: &str,
        predicate: &str,
    ) -> Result<()> {
        for (batch_idx, batch) in self.batches.iter().enumerate() {
            let sources = batch
                .column(rel_col::SOURCE_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("source_id");
            let targets = batch
                .column(rel_col::TARGET_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("target_id");
            let predicates = batch
                .column(rel_col::PREDICATE)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("predicate");
            let deleted = batch
                .column(rel_col::DELETED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("deleted");

            for i in 0..batch.num_rows() {
                if sources.value(i) == source_id
                    && targets.value(i) == target_id
                    && predicates.value(i) == predicate
                    && !deleted.value(i)
                {
                    // Rebuild with deleted=true for this row
                    let mut columns: Vec<Arc<dyn Array>> = Vec::new();
                    for col_idx in 0..batch.num_columns() {
                        if col_idx == rel_col::DELETED {
                            let mut new_deleted: Vec<bool> =
                                (0..batch.num_rows()).map(|j| deleted.value(j)).collect();
                            new_deleted[i] = true;
                            columns.push(Arc::new(BooleanArray::from(new_deleted)));
                        } else {
                            columns.push(batch.column(col_idx).clone());
                        }
                    }
                    let new_batch = RecordBatch::try_new(self.schema.clone(), columns)?;
                    self.batches[batch_idx] = new_batch;
                    return Ok(());
                }
            }
        }

        Err(RelationError::NotFound(
            source_id.to_string(),
            target_id.to_string(),
            predicate.to_string(),
        ))
    }

    /// Query all relations for an item (as source OR target).
    pub fn query_relations(&self, item_id: &str) -> Vec<RecordBatch> {
        let mut results = Vec::new();

        for batch in &self.batches {
            let sources = batch
                .column(rel_col::SOURCE_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("source_id");
            let targets = batch
                .column(rel_col::TARGET_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("target_id");
            let deleted = batch
                .column(rel_col::DELETED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("deleted");

            for i in 0..batch.num_rows() {
                if deleted.value(i) {
                    continue;
                }
                if sources.value(i) == item_id || targets.value(i) == item_id {
                    results.push(batch.slice(i, 1));
                }
            }
        }

        results
    }

    /// Access the relations schema.
    pub fn schema(&self) -> &arrow::datatypes::Schema {
        &self.schema
    }

    /// Access the underlying relation batches.
    pub fn batches(&self) -> &[RecordBatch] {
        &self.batches
    }

    /// Count active (non-deleted) relations.
    pub fn active_count(&self) -> usize {
        let mut count = 0;
        for batch in &self.batches {
            let deleted = batch
                .column(rel_col::DELETED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("deleted");
            for i in 0..batch.num_rows() {
                if !deleted.value(i) {
                    count += 1;
                }
            }
        }
        count
    }
}

impl Default for RelationsStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_query_relation() {
        let mut store = RelationsStore::new();
        store
            .add_relation("EXP-1257", "VOY-145", "implements")
            .unwrap();

        let rels = store.query_relations("EXP-1257");
        assert_eq!(rels.len(), 1);

        // Also findable from target side
        let rels = store.query_relations("VOY-145");
        assert_eq!(rels.len(), 1);
    }

    #[test]
    fn test_multiple_relations() {
        let mut store = RelationsStore::new();
        store
            .add_relation("EXP-1257", "VOY-145", "implements")
            .unwrap();
        store
            .add_relation("EXP-1257", "EXP-1258", "blocks")
            .unwrap();
        store
            .add_relation("EXP-1260", "VOY-145", "implements")
            .unwrap();

        let rels = store.query_relations("EXP-1257");
        assert_eq!(rels.len(), 2); // implements + blocks

        let rels = store.query_relations("VOY-145");
        assert_eq!(rels.len(), 2); // two implements
    }

    #[test]
    fn test_remove_relation() {
        let mut store = RelationsStore::new();
        store
            .add_relation("EXP-1257", "VOY-145", "implements")
            .unwrap();
        assert_eq!(store.active_count(), 1);

        store
            .remove_relation("EXP-1257", "VOY-145", "implements")
            .unwrap();
        assert_eq!(store.active_count(), 0);

        // Removed relation should not appear in queries
        let rels = store.query_relations("EXP-1257");
        assert_eq!(rels.len(), 0);
    }

    #[test]
    fn test_remove_nonexistent_relation() {
        let mut store = RelationsStore::new();
        let err = store.remove_relation("EXP-1", "VOY-1", "blocks");
        assert!(err.is_err());
    }

    #[test]
    fn test_bidirectional_query() {
        let mut store = RelationsStore::new();
        store.add_relation("EXPR-131", "EXP-800", "spawns").unwrap();

        // Queryable from both sides
        assert_eq!(store.query_relations("EXPR-131").len(), 1);
        assert_eq!(store.query_relations("EXP-800").len(), 1);

        // Not found for unrelated item
        assert_eq!(store.query_relations("EXP-999").len(), 0);
    }
}
