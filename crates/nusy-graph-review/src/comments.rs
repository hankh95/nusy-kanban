use arrow::array::{Array, BooleanArray, RecordBatch, StringArray, TimestampMillisecondArray};
use arrow::datatypes::Schema;
use std::sync::Arc;

use crate::schema::{comments_col, comments_schema};

// ── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum CommentError {
    #[error("Comment not found: {0}")]
    NotFound(String),

    #[error("Internal error: {0}")]
    InternalError(String),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

pub type Result<T> = std::result::Result<T, CommentError>;

// ── CommentStore ────────────────────────────────────────────────────────────

pub struct CommentStore {
    comments_batches: Vec<RecordBatch>,
    comments_schema: Arc<Schema>,
}

impl CommentStore {
    pub fn new() -> Self {
        Self {
            comments_batches: Vec::new(),
            comments_schema: comments_schema(),
        }
    }

    pub fn comments_batches(&self) -> &[RecordBatch] {
        &self.comments_batches
    }

    pub fn comments_schema(&self) -> &Arc<Schema> {
        &self.comments_schema
    }

    pub fn load_comments(&mut self, batches: Vec<RecordBatch>) {
        self.comments_batches = batches;
    }

    // ── Add comment ─────────────────────────────────────────────────────

    pub fn add_comment(
        &mut self,
        proposal_id: &str,
        reviewer: &str,
        body: &str,
        line_ref: Option<&str>,
        parent_comment_id: Option<&str>,
    ) -> Result<String> {
        let comment_id = format!("CMT-{:03}", self.total_count() + 1);
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            self.comments_schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![comment_id.as_str()])),
                Arc::new(StringArray::from(vec![proposal_id])),
                Arc::new(StringArray::from(vec![parent_comment_id])),
                Arc::new(StringArray::from(vec![reviewer])),
                Arc::new(StringArray::from(vec![body])),
                Arc::new(StringArray::from(vec![line_ref])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(BooleanArray::from(vec![false])),
            ],
        )?;

        self.comments_batches.push(batch);
        Ok(comment_id)
    }

    // ── Resolve comment ─────────────────────────────────────────────────

    pub fn resolve_comment(&mut self, comment_id: &str) -> Result<()> {
        let (batch_idx, row_idx) = self.find_comment(comment_id)?;
        let batch = &self.comments_batches[batch_idx];
        let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());

        for ci in 0..batch.num_columns() {
            if ci == comments_col::RESOLVED {
                let old = batch
                    .column(ci)
                    .as_any()
                    .downcast_ref::<BooleanArray>()
                    .ok_or_else(|| {
                        CommentError::InternalError("resolved column downcast".into())
                    })?;
                let vals: Vec<bool> = (0..batch.num_rows())
                    .map(|i| if i == row_idx { true } else { old.value(i) })
                    .collect();
                columns.push(Arc::new(BooleanArray::from(vals)));
            } else {
                columns.push(batch.column(ci).clone());
            }
        }

        self.comments_batches[batch_idx] =
            RecordBatch::try_new(self.comments_schema.clone(), columns)?;
        Ok(())
    }

    // ── List comments for a proposal ────────────────────────────────────

    pub fn list_comments(&self, proposal_id: &str) -> Result<Vec<RecordBatch>> {
        let mut result = Vec::new();
        for batch in &self.comments_batches {
            let prop_ids = batch
                .column(comments_col::PROPOSAL_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| CommentError::InternalError("proposal_id column downcast".into()))?;
            for i in 0..batch.num_rows() {
                if prop_ids.value(i) == proposal_id {
                    let row_batch = batch.slice(i, 1);
                    result.push(row_batch);
                }
            }
        }
        Ok(result)
    }

    /// Count unresolved top-level comment threads for a proposal.
    ///
    /// A thread is unresolved if the root comment (parent_comment_id is NULL)
    /// has resolved=false.
    pub fn unresolved_count(&self, proposal_id: &str) -> Result<usize> {
        let mut count = 0;
        for batch in &self.comments_batches {
            let prop_ids = batch
                .column(comments_col::PROPOSAL_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| CommentError::InternalError("proposal_id column downcast".into()))?;
            let parents = batch
                .column(comments_col::PARENT_COMMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| {
                    CommentError::InternalError("parent_comment_id column downcast".into())
                })?;
            let resolved = batch
                .column(comments_col::RESOLVED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .ok_or_else(|| CommentError::InternalError("resolved column downcast".into()))?;

            for i in 0..batch.num_rows() {
                if prop_ids.value(i) == proposal_id && parents.is_null(i) && !resolved.value(i) {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    // ── Internal helpers ────────────────────────────────────────────────

    fn total_count(&self) -> usize {
        self.comments_batches.iter().map(|b| b.num_rows()).sum()
    }

    fn find_comment(&self, comment_id: &str) -> Result<(usize, usize)> {
        for (batch_idx, batch) in self.comments_batches.iter().enumerate() {
            let ids = batch
                .column(comments_col::COMMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| CommentError::InternalError("comment_id column downcast".into()))?;
            for row_idx in 0..batch.num_rows() {
                if ids.value(row_idx) == comment_id {
                    return Ok((batch_idx, row_idx));
                }
            }
        }
        Err(CommentError::NotFound(comment_id.to_string()))
    }
}

impl Default for CommentStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_comment() {
        let mut store = CommentStore::new();
        let id = store
            .add_comment("PROP-001", "captain", "Needs more evidence", None, None)
            .unwrap();
        assert_eq!(id, "CMT-001");
        assert_eq!(store.total_count(), 1);
    }

    #[test]
    fn test_threaded_reply() {
        let mut store = CommentStore::new();
        let root = store
            .add_comment("PROP-001", "captain", "Fix the naming", None, None)
            .unwrap();
        let reply = store
            .add_comment(
                "PROP-001",
                "being-alpha",
                "Done, renamed to X",
                None,
                Some(&root),
            )
            .unwrap();
        assert_eq!(reply, "CMT-002");
        assert_eq!(store.total_count(), 2);
    }

    #[test]
    fn test_unresolved_count() {
        let mut store = CommentStore::new();

        // Two root comments on PROP-001
        let c1 = store
            .add_comment("PROP-001", "captain", "Issue 1", None, None)
            .unwrap();
        let _c2 = store
            .add_comment("PROP-001", "captain", "Issue 2", None, None)
            .unwrap();
        // A reply (not a root thread — shouldn't count)
        let _reply = store
            .add_comment("PROP-001", "alpha", "Fixed", None, Some(&c1))
            .unwrap();
        // A comment on a different proposal
        let _other = store
            .add_comment("PROP-002", "captain", "Other issue", None, None)
            .unwrap();

        assert_eq!(store.unresolved_count("PROP-001").unwrap(), 2);
        assert_eq!(store.unresolved_count("PROP-002").unwrap(), 1);
    }

    #[test]
    fn test_resolve_reduces_count() {
        let mut store = CommentStore::new();
        let c1 = store
            .add_comment("PROP-001", "captain", "Fix this", None, None)
            .unwrap();
        let _c2 = store
            .add_comment("PROP-001", "captain", "And this", None, None)
            .unwrap();

        assert_eq!(store.unresolved_count("PROP-001").unwrap(), 2);

        store.resolve_comment(&c1).unwrap();
        assert_eq!(store.unresolved_count("PROP-001").unwrap(), 1);
    }

    #[test]
    fn test_list_comments_filters_by_proposal() {
        let mut store = CommentStore::new();
        store
            .add_comment("PROP-001", "captain", "Comment A", None, None)
            .unwrap();
        store
            .add_comment("PROP-002", "captain", "Comment B", None, None)
            .unwrap();
        store
            .add_comment("PROP-001", "alpha", "Comment C", None, None)
            .unwrap();

        let comments = store.list_comments("PROP-001").unwrap();
        assert_eq!(comments.len(), 2);

        let comments = store.list_comments("PROP-002").unwrap();
        assert_eq!(comments.len(), 1);
    }

    #[test]
    fn test_resolve_nonexistent_comment() {
        let mut store = CommentStore::new();
        let err = store.resolve_comment("CMT-999").unwrap_err();
        assert!(matches!(err, CommentError::NotFound(_)));
    }

    #[test]
    fn test_comment_with_line_ref() {
        let mut store = CommentStore::new();
        let id = store
            .add_comment(
                "PROP-001",
                "captain",
                "This triple conflicts",
                Some("being:alpha/knows/calculus"),
                None,
            )
            .unwrap();
        assert_eq!(id, "CMT-001");

        let comments = store.list_comments("PROP-001").unwrap();
        assert_eq!(comments.len(), 1);
        let line_refs = comments[0]
            .column(comments_col::LINE_REF)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(line_refs.value(0), "being:alpha/knows/calculus");
    }
}
