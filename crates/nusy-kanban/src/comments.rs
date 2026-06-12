//! Item comments — threaded, resolvable comments stored in Arrow RecordBatches.
//!
//! Replaces the legacy hack of storing comments as runs with `to_status="comment"`.
//! Comments are stored in a separate CommentsTable with proper threading and
//! resolution tracking.

use crate::schema::{cmt_col, comments_schema};
use arrow::array::{Array, BooleanArray, RecordBatch, StringArray, TimestampMillisecondArray};
use arrow::datatypes::SchemaRef;
use std::sync::Arc;

/// A comment read from the Arrow store.
#[derive(Debug, Clone)]
pub struct Comment {
    pub comment_id: String,
    pub item_id: String,
    pub author: String,
    pub body: String,
    pub created_at_ms: i64,
    pub parent_comment_id: Option<String>,
    pub resolved: bool,
}

/// Arrow-backed store for item comments.
pub struct CommentsStore {
    batches: Vec<RecordBatch>,
    schema: SchemaRef,
    /// Next sequence number per item (for CMT ID generation).
    next_seq: std::collections::HashMap<String, u32>,
}

impl CommentsStore {
    pub fn new() -> Self {
        Self {
            batches: Vec::new(),
            schema: comments_schema(),
            next_seq: std::collections::HashMap::new(),
        }
    }

    /// Add a comment to an item. Returns the allocated comment ID.
    pub fn add_comment(
        &mut self,
        item_id: &str,
        author: &str,
        body: &str,
        parent_comment_id: Option<&str>,
    ) -> Result<String, arrow::error::ArrowError> {
        let seq = self.next_seq.entry(item_id.to_string()).or_insert(0);
        *seq += 1;
        let comment_id = format!("CMT-{}-{:03}", item_id, seq);
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![comment_id.as_str()])),
                Arc::new(StringArray::from(vec![item_id])),
                Arc::new(StringArray::from(vec![author])),
                Arc::new(StringArray::from(vec![body])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![parent_comment_id])),
                Arc::new(BooleanArray::from(vec![false])),
            ],
        )?;

        self.batches.push(batch);
        Ok(comment_id)
    }

    /// List all comments for an item, ordered by created_at.
    pub fn list_comments(&self, item_id: &str) -> Vec<Comment> {
        let mut comments = Vec::new();

        for batch in &self.batches {
            let ids = col_str(batch, cmt_col::COMMENT_ID);
            let item_ids = col_str(batch, cmt_col::ITEM_ID);
            let authors = col_str(batch, cmt_col::AUTHOR);
            let bodies = col_str(batch, cmt_col::BODY);
            let timestamps = batch
                .column(cmt_col::CREATED_AT)
                .as_any()
                .downcast_ref::<TimestampMillisecondArray>()
                .expect("created_at column");
            let parents = col_str(batch, cmt_col::PARENT_COMMENT_ID);
            let resolved = batch
                .column(cmt_col::RESOLVED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("resolved column");

            for i in 0..batch.num_rows() {
                if item_ids.value(i) != item_id {
                    continue;
                }
                comments.push(Comment {
                    comment_id: ids.value(i).to_string(),
                    item_id: item_ids.value(i).to_string(),
                    author: authors.value(i).to_string(),
                    body: bodies.value(i).to_string(),
                    created_at_ms: timestamps.value(i),
                    parent_comment_id: if parents.is_null(i) {
                        None
                    } else {
                        Some(parents.value(i).to_string())
                    },
                    resolved: resolved.value(i),
                });
            }
        }

        comments.sort_by_key(|c| c.created_at_ms);
        comments
    }

    /// Resolve a comment by ID. Returns true if found.
    pub fn resolve_comment(&mut self, comment_id: &str) -> bool {
        self.set_resolved(comment_id, true)
    }

    /// Unresolve a comment by ID. Returns true if found.
    pub fn unresolve_comment(&mut self, comment_id: &str) -> bool {
        self.set_resolved(comment_id, false)
    }

    /// Get all batches (for persistence).
    pub fn batches(&self) -> &[RecordBatch] {
        &self.batches
    }

    /// Get the comments schema.
    pub fn schema(&self) -> &arrow::datatypes::Schema {
        &self.schema
    }

    /// Load pre-existing comment batches (from Parquet).
    pub fn load(&mut self, batches: Vec<RecordBatch>) {
        // Rebuild next_seq from loaded data
        for batch in &batches {
            let item_ids = col_str(batch, cmt_col::ITEM_ID);
            for i in 0..batch.num_rows() {
                let entry = self
                    .next_seq
                    .entry(item_ids.value(i).to_string())
                    .or_insert(0);
                *entry += 1;
            }
        }
        self.batches = batches;
    }

    /// Migrate legacy comments from the runs table.
    ///
    /// Scans for runs where `to_status == "comment"` and creates proper comment
    /// entries. Does NOT delete original run rows (they're audit history).
    pub fn migrate_from_runs(&mut self, runs_batches: &[RecordBatch]) {
        use crate::schema::runs_col;

        for batch in runs_batches {
            let to_statuses = batch
                .column(runs_col::TO_STATUS)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("to_status column");
            let item_ids = batch
                .column(runs_col::ITEM_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("item_id column");
            let agents = batch
                .column(runs_col::BY_AGENT)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("by_agent column");
            let reasons = batch
                .column(runs_col::REASON)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("reason column");
            let timestamps = batch
                .column(runs_col::TIMESTAMP)
                .as_any()
                .downcast_ref::<TimestampMillisecondArray>()
                .expect("timestamp column");

            for i in 0..batch.num_rows() {
                if to_statuses.value(i) != "comment" {
                    continue;
                }

                let item_id = item_ids.value(i);
                let author = if agents.is_null(i) {
                    "unknown"
                } else {
                    agents.value(i)
                };
                let body = if reasons.is_null(i) {
                    ""
                } else {
                    reasons.value(i)
                };
                if body.is_empty() {
                    continue;
                }

                let seq = self.next_seq.entry(item_id.to_string()).or_insert(0);
                *seq += 1;
                let comment_id = format!("CMT-{}-{:03}", item_id, seq);
                let ts = timestamps.value(i);

                let migrated_batch = RecordBatch::try_new(
                    self.schema.clone(),
                    vec![
                        Arc::new(StringArray::from(vec![comment_id.as_str()])),
                        Arc::new(StringArray::from(vec![item_id])),
                        Arc::new(StringArray::from(vec![author])),
                        Arc::new(StringArray::from(vec![body])),
                        Arc::new(TimestampMillisecondArray::from(vec![ts]).with_timezone("UTC")),
                        Arc::new(StringArray::from(vec![None::<&str>])), // no parent
                        Arc::new(BooleanArray::from(vec![false])),
                    ],
                )
                .expect("migrate comment batch");

                self.batches.push(migrated_batch);
            }
        }
    }

    /// Total comment count.
    pub fn len(&self) -> usize {
        self.batches.iter().map(|b| b.num_rows()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // ── Private helpers ─────────────────────────────────────────────────────

    fn set_resolved(&mut self, comment_id: &str, value: bool) -> bool {
        for batch_idx in 0..self.batches.len() {
            let batch = &self.batches[batch_idx];
            let ids = col_str(batch, cmt_col::COMMENT_ID);

            for i in 0..batch.num_rows() {
                if ids.value(i) != comment_id {
                    continue;
                }

                // Rebuild batch with updated resolved flag
                let resolved = batch
                    .column(cmt_col::RESOLVED)
                    .as_any()
                    .downcast_ref::<BooleanArray>()
                    .expect("resolved column");
                let mut new_resolved: Vec<bool> =
                    (0..batch.num_rows()).map(|j| resolved.value(j)).collect();
                new_resolved[i] = value;

                let mut columns: Vec<Arc<dyn Array>> = Vec::new();
                for col_idx in 0..batch.num_columns() {
                    if col_idx == cmt_col::RESOLVED {
                        columns.push(Arc::new(BooleanArray::from(new_resolved.clone())));
                    } else {
                        columns.push(batch.column(col_idx).clone());
                    }
                }

                let new_batch =
                    RecordBatch::try_new(self.schema.clone(), columns).expect("rebuild batch");
                self.batches[batch_idx] = new_batch;
                return true;
            }
        }
        false
    }
}

impl Default for CommentsStore {
    fn default() -> Self {
        Self::new()
    }
}

fn col_str(batch: &RecordBatch, col: usize) -> &StringArray {
    batch
        .column(col)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("string column")
}

/// Format comments for display in `format_item_detail()`.
pub fn format_comments(comments: &[Comment]) -> String {
    if comments.is_empty() {
        return String::new();
    }

    let mut lines = Vec::new();
    lines.push(String::new());
    lines.push(format!("  Comments ({}):", comments.len()));

    for c in comments {
        let resolved_tag = if c.resolved { " [resolved]" } else { "" };
        let date = chrono::DateTime::from_timestamp_millis(c.created_at_ms)
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();

        let indent = if c.parent_comment_id.is_some() {
            "      "
        } else {
            "    "
        };
        lines.push(format!(
            "{indent}[{}] @{} ({date}){resolved_tag}:",
            c.comment_id, c.author
        ));

        // Indent body lines
        for line in c.body.lines() {
            lines.push(format!("{indent}  {line}"));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_comment() {
        let mut store = CommentsStore::new();
        let id = store
            .add_comment("EX-3244", "Mini", "Test comment", None)
            .expect("add comment");

        assert_eq!(id, "CMT-EX-3244-001");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_add_multiple_comments() {
        let mut store = CommentsStore::new();
        let id1 = store
            .add_comment("EX-3244", "Mini", "First", None)
            .expect("add");
        let id2 = store
            .add_comment("EX-3244", "M5", "Second", None)
            .expect("add");
        let id3 = store
            .add_comment("EX-3244", "DGX", "Reply", Some(&id1))
            .expect("add");

        assert_eq!(id1, "CMT-EX-3244-001");
        assert_eq!(id2, "CMT-EX-3244-002");
        assert_eq!(id3, "CMT-EX-3244-003");
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn test_list_comments_filtered_by_item() {
        let mut store = CommentsStore::new();
        store
            .add_comment("EX-100", "Mini", "Comment on 100", None)
            .expect("add");
        store
            .add_comment("EX-200", "M5", "Comment on 200", None)
            .expect("add");
        store
            .add_comment("EX-100", "DGX", "Another on 100", None)
            .expect("add");

        let comments = store.list_comments("EX-100");
        assert_eq!(comments.len(), 2);
        assert!(comments.iter().all(|c| c.item_id == "EX-100"));
    }

    #[test]
    fn test_list_comments_ordered_by_time() {
        let mut store = CommentsStore::new();
        store
            .add_comment("EX-100", "A", "First", None)
            .expect("add");
        store
            .add_comment("EX-100", "B", "Second", None)
            .expect("add");

        let comments = store.list_comments("EX-100");
        assert!(comments[0].created_at_ms <= comments[1].created_at_ms);
    }

    #[test]
    fn test_resolve_comment() {
        let mut store = CommentsStore::new();
        let id = store
            .add_comment("EX-100", "Mini", "Review comment", None)
            .expect("add");

        assert!(store.resolve_comment(&id));

        let comments = store.list_comments("EX-100");
        assert!(comments[0].resolved);
    }

    #[test]
    fn test_unresolve_comment() {
        let mut store = CommentsStore::new();
        let id = store
            .add_comment("EX-100", "Mini", "Comment", None)
            .expect("add");

        store.resolve_comment(&id);
        store.unresolve_comment(&id);

        let comments = store.list_comments("EX-100");
        assert!(!comments[0].resolved);
    }

    #[test]
    fn test_resolve_nonexistent_returns_false() {
        let mut store = CommentsStore::new();
        assert!(!store.resolve_comment("CMT-NONEXISTENT-001"));
    }

    #[test]
    fn test_empty_store() {
        let store = CommentsStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert!(store.list_comments("EX-100").is_empty());
    }

    #[test]
    fn test_threaded_comments() {
        let mut store = CommentsStore::new();
        let parent = store
            .add_comment("EX-100", "Mini", "Top-level", None)
            .expect("add");
        store
            .add_comment("EX-100", "M5", "Reply", Some(&parent))
            .expect("add");

        let comments = store.list_comments("EX-100");
        assert_eq!(comments.len(), 2);
        assert!(comments[0].parent_comment_id.is_none());
        assert_eq!(
            comments[1].parent_comment_id.as_deref(),
            Some(parent.as_str())
        );
    }

    #[test]
    fn test_format_comments_empty() {
        assert!(format_comments(&[]).is_empty());
    }

    #[test]
    fn test_format_comments_with_data() {
        let comments = vec![Comment {
            comment_id: "CMT-EX-100-001".to_string(),
            item_id: "EX-100".to_string(),
            author: "Mini".to_string(),
            body: "Test comment".to_string(),
            created_at_ms: 1710374400000,
            parent_comment_id: None,
            resolved: false,
        }];

        let output = format_comments(&comments);
        assert!(output.contains("Comments (1)"));
        assert!(output.contains("CMT-EX-100-001"));
        assert!(output.contains("@Mini"));
        assert!(output.contains("Test comment"));
    }

    #[test]
    fn test_format_comments_resolved() {
        let comments = vec![Comment {
            comment_id: "CMT-EX-100-001".to_string(),
            item_id: "EX-100".to_string(),
            author: "Mini".to_string(),
            body: "Resolved comment".to_string(),
            created_at_ms: 1710374400000,
            parent_comment_id: None,
            resolved: true,
        }];

        let output = format_comments(&comments);
        assert!(output.contains("[resolved]"));
    }

    #[test]
    fn test_migrate_from_runs() {
        use crate::schema::runs_schema;

        // Create a runs batch with a legacy comment
        let runs_batch = RecordBatch::try_new(
            runs_schema(),
            vec![
                Arc::new(StringArray::from(vec!["run-001"])),
                Arc::new(StringArray::from(vec!["EX-100"])),
                Arc::new(StringArray::from(vec![None::<&str>])), // from_status
                Arc::new(StringArray::from(vec!["comment"])),    // to_status
                Arc::new(
                    TimestampMillisecondArray::from(vec![1710374400000i64]).with_timezone("UTC"),
                ),
                Arc::new(StringArray::from(vec![Some("Mini")])), // agent
                Arc::new(BooleanArray::from(vec![false])),       // forced
                Arc::new(StringArray::from(vec![Some("Legacy comment text")])), // reason = body
            ],
        )
        .expect("create runs batch");

        let mut store = CommentsStore::new();
        store.migrate_from_runs(&[runs_batch]);

        assert_eq!(store.len(), 1);
        let comments = store.list_comments("EX-100");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "Mini");
        assert_eq!(comments[0].body, "Legacy comment text");
        assert_eq!(comments[0].created_at_ms, 1710374400000);
    }

    #[test]
    fn test_migrate_skips_non_comment_runs() {
        use crate::schema::runs_schema;

        let runs_batch = RecordBatch::try_new(
            runs_schema(),
            vec![
                Arc::new(StringArray::from(vec!["run-001", "run-002"])),
                Arc::new(StringArray::from(vec!["EX-100", "EX-100"])),
                Arc::new(StringArray::from(vec![Some("backlog"), None::<&str>])),
                Arc::new(StringArray::from(vec!["in_progress", "comment"])),
                Arc::new(
                    TimestampMillisecondArray::from(vec![1000i64, 2000i64]).with_timezone("UTC"),
                ),
                Arc::new(StringArray::from(vec![Some("M5"), Some("Mini")])),
                Arc::new(BooleanArray::from(vec![false, false])),
                Arc::new(StringArray::from(vec![None::<&str>, Some("A comment")])),
            ],
        )
        .expect("create runs batch");

        let mut store = CommentsStore::new();
        store.migrate_from_runs(&[runs_batch]);

        // Only the "comment" row should be migrated
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_load_rebuilds_seq() {
        let mut store = CommentsStore::new();
        store
            .add_comment("EX-100", "A", "First", None)
            .expect("add");
        store
            .add_comment("EX-100", "B", "Second", None)
            .expect("add");

        let batches = store.batches().to_vec();

        let mut loaded = CommentsStore::new();
        loaded.load(batches);

        // New comment should get seq 3 (not 1)
        let id = loaded
            .add_comment("EX-100", "C", "Third", None)
            .expect("add");
        assert_eq!(id, "CMT-EX-100-003");
    }
}
