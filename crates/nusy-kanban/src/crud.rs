//! CRUD operations on the kanban items store.
//!
//! All mutations operate on Arrow RecordBatches stored in the KanbanStore.
//! Each mutation can optionally create a git commit via nusy-arrow-git.

use crate::id_alloc;
use crate::item_type::ItemType;
use crate::schema::{comments_schema, items_col, items_schema, runs_schema};
use arrow::array::{
    Array, BooleanArray, Int32Array, ListArray, ListBuilder, RecordBatch, StringArray,
    StringBuilder, TimestampMillisecondArray,
};
use std::sync::Arc;

/// Errors from CRUD operations.
#[derive(Debug, thiserror::Error)]
pub enum CrudError {
    #[error("Item not found: {0}")]
    NotFound(String),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("ID allocation error: {0}")]
    IdAlloc(#[from] crate::id_alloc::IdAllocError),

    #[error("State machine error: {0}")]
    State(#[from] crate::state_machine::StateError),

    #[error("Duplicate ID: {0} already exists")]
    DuplicateId(String),
}

pub type Result<T> = std::result::Result<T, CrudError>;

/// Input for creating a new kanban item.
#[derive(Debug, Clone)]
pub struct CreateItemInput {
    pub title: String,
    pub item_type: ItemType,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub tags: Vec<String>,
    pub related: Vec<String>,
    pub depends_on: Vec<String>,
    pub body: Option<String>,
}

/// The in-memory kanban store — holds items, runs, and comments as RecordBatches.
pub struct KanbanStore {
    items_batches: Vec<RecordBatch>,
    runs_batches: Vec<RecordBatch>,
    comments_batches: Vec<RecordBatch>,
    items_schema: Arc<arrow::datatypes::Schema>,
    runs_schema: Arc<arrow::datatypes::Schema>,
    comments_schema: Arc<arrow::datatypes::Schema>,
}

impl KanbanStore {
    pub fn new() -> Self {
        KanbanStore {
            items_batches: Vec::new(),
            runs_batches: Vec::new(),
            comments_batches: Vec::new(),
            items_schema: items_schema(),
            runs_schema: runs_schema(),
            comments_schema: comments_schema(),
        }
    }

    /// All items batches (for querying).
    pub fn items_batches(&self) -> &[RecordBatch] {
        &self.items_batches
    }

    /// All runs batches (for querying).
    pub fn runs_batches(&self) -> &[RecordBatch] {
        &self.runs_batches
    }

    /// Get the items schema.
    pub fn items_schema(&self) -> &arrow::datatypes::Schema {
        &self.items_schema
    }

    /// Get the runs schema.
    pub fn runs_schema(&self) -> &arrow::datatypes::Schema {
        &self.runs_schema
    }

    /// Load pre-existing items batches (from Parquet).
    pub fn load_items(&mut self, batches: Vec<RecordBatch>) {
        self.items_batches = batches;
    }

    /// Load pre-existing runs batches (from Parquet).
    pub fn load_runs(&mut self, batches: Vec<RecordBatch>) {
        self.runs_batches = batches;
    }

    /// All comments batches (for persistence/querying).
    pub fn comments_batches(&self) -> &[RecordBatch] {
        &self.comments_batches
    }

    /// Get the comments schema.
    pub fn comments_schema(&self) -> &arrow::datatypes::Schema {
        &self.comments_schema
    }

    /// Load pre-existing comments batches (from Parquet).
    pub fn load_comments(&mut self, batches: Vec<RecordBatch>) {
        self.comments_batches = batches;
    }

    /// List all comments for an item, ordered by created_at.
    pub fn list_comments(&self, item_id: &str) -> Vec<crate::comments::Comment> {
        use crate::schema::cmt_col;

        let mut comments = Vec::new();
        for batch in &self.comments_batches {
            let item_ids = batch
                .column(cmt_col::ITEM_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("item_id column");
            let ids = batch
                .column(cmt_col::COMMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("comment_id column");
            let authors = batch
                .column(cmt_col::AUTHOR)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("author column");
            let bodies = batch
                .column(cmt_col::BODY)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("body column");
            let timestamps = batch
                .column(cmt_col::CREATED_AT)
                .as_any()
                .downcast_ref::<TimestampMillisecondArray>()
                .expect("created_at column");
            let parents = batch
                .column(cmt_col::PARENT_COMMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("parent_comment_id column");
            let resolved = batch
                .column(cmt_col::RESOLVED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("resolved column");

            for i in 0..batch.num_rows() {
                if item_ids.value(i) != item_id {
                    continue;
                }
                comments.push(crate::comments::Comment {
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

    /// Total number of items (including deleted).
    pub fn item_count(&self) -> usize {
        self.items_batches.iter().map(|b| b.num_rows()).sum()
    }

    /// Total number of active (non-deleted) items.
    pub fn active_item_count(&self) -> usize {
        let mut count = 0;
        for batch in &self.items_batches {
            let deleted = batch
                .column(items_col::DELETED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("deleted column");
            for i in 0..batch.num_rows() {
                if !deleted.value(i) {
                    count += 1;
                }
            }
        }
        count
    }

    /// Create a new item. Returns the allocated ID.
    pub fn create_item(&mut self, input: &CreateItemInput) -> Result<String> {
        let id = id_alloc::allocate_id(&self.items_batches, input.item_type);
        let board = input.item_type.board();
        let now_ms = chrono::Utc::now().timestamp_millis();

        // Build list arrays
        let mut tags_builder = ListBuilder::new(StringBuilder::new());
        for tag in &input.tags {
            tags_builder.values().append_value(tag);
        }
        tags_builder.append(true);

        let mut related_builder = ListBuilder::new(StringBuilder::new());
        for rel in &input.related {
            related_builder.values().append_value(rel);
        }
        related_builder.append(true);

        let mut depends_builder = ListBuilder::new(StringBuilder::new());
        for dep in &input.depends_on {
            depends_builder.values().append_value(dep);
        }
        depends_builder.append(true);

        // Compute body_hash if body is present
        let body_hash = input.body.as_ref().map(|b| {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(b.as_bytes()))
        });

        let batch = RecordBatch::try_new(
            self.items_schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![id.as_str()])),
                Arc::new(StringArray::from(vec![input.title.as_str()])),
                Arc::new(StringArray::from(vec![input.item_type.as_str()])),
                Arc::new(StringArray::from(vec!["backlog"])),
                Arc::new(StringArray::from(vec![input.priority.as_deref()])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![input.assignee.as_deref()])),
                Arc::new(StringArray::from(vec![board])),
                Arc::new(tags_builder.finish()),
                Arc::new(related_builder.finish()),
                Arc::new(depends_builder.finish()),
                Arc::new(StringArray::from(vec![input.body.as_deref()])),
                Arc::new(StringArray::from(vec![body_hash.as_deref()])),
                Arc::new(BooleanArray::from(vec![false])),
                Arc::new(StringArray::from(vec![None::<&str>])), // resolution
                Arc::new(StringArray::from(vec![None::<&str>])), // closed_by
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")), // updated_at = created_at
                Arc::new(arrow::array::Int32Array::from(vec![None::<i32>])), // priority_rank
            ],
        )?;

        self.items_batches.push(batch);

        // Record creation in runs table
        self.record_run(&id, None, "backlog", None, false, None)?;

        Ok(id)
    }

    /// Create a new item with a specific ID (for paper-scoped IDs like H130.1).
    /// Returns the ID. Errors if the ID already exists.
    pub fn create_item_with_id(&mut self, id: &str, input: &CreateItemInput) -> Result<String> {
        // Guard against duplicate IDs
        if self.get_item(id).is_ok() {
            return Err(CrudError::DuplicateId(id.to_string()));
        }

        let board = input.item_type.board();
        let now_ms = chrono::Utc::now().timestamp_millis();

        let mut tags_builder = ListBuilder::new(StringBuilder::new());
        for tag in &input.tags {
            tags_builder.values().append_value(tag);
        }
        tags_builder.append(true);

        let mut related_builder = ListBuilder::new(StringBuilder::new());
        for rel in &input.related {
            related_builder.values().append_value(rel);
        }
        related_builder.append(true);

        let mut depends_builder = ListBuilder::new(StringBuilder::new());
        for dep in &input.depends_on {
            depends_builder.values().append_value(dep);
        }
        depends_builder.append(true);

        // Compute body_hash if body is present
        let body_hash = input.body.as_ref().map(|b| {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(b.as_bytes()))
        });

        let batch = RecordBatch::try_new(
            self.items_schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![id])),
                Arc::new(StringArray::from(vec![input.title.as_str()])),
                Arc::new(StringArray::from(vec![input.item_type.as_str()])),
                Arc::new(StringArray::from(vec!["backlog"])),
                Arc::new(StringArray::from(vec![input.priority.as_deref()])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![input.assignee.as_deref()])),
                Arc::new(StringArray::from(vec![board])),
                Arc::new(tags_builder.finish()),
                Arc::new(related_builder.finish()),
                Arc::new(depends_builder.finish()),
                Arc::new(StringArray::from(vec![input.body.as_deref()])),
                Arc::new(StringArray::from(vec![body_hash.as_deref()])),
                Arc::new(BooleanArray::from(vec![false])),
                Arc::new(StringArray::from(vec![None::<&str>])), // resolution
                Arc::new(StringArray::from(vec![None::<&str>])), // closed_by
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")), // updated_at
                Arc::new(arrow::array::Int32Array::from(vec![None::<i32>])), // priority_rank
            ],
        )?;

        self.items_batches.push(batch);
        self.record_run(id, None, "backlog", None, false, None)?;
        Ok(id.to_string())
    }

    /// Get an item by ID. Returns the row data as a single-row RecordBatch.
    pub fn get_item(&self, id: &str) -> Result<RecordBatch> {
        for batch in &self.items_batches {
            let ids = batch
                .column(items_col::ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("id column");
            let deleted = batch
                .column(items_col::DELETED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("deleted column");

            for i in 0..batch.num_rows() {
                if ids.value(i) == id && !deleted.value(i) {
                    return Ok(batch.slice(i, 1));
                }
            }
        }
        Err(CrudError::NotFound(id.to_string()))
    }

    /// Update an item's status. Returns the old status.
    pub fn update_status(
        &mut self,
        id: &str,
        new_status: &str,
        agent: Option<&str>,
        forced: bool,
        reason: Option<&str>,
    ) -> Result<String> {
        let (batch_idx, row_idx, old_status) = self.find_item_mut(id)?;
        let batch = &self.items_batches[batch_idx];

        // Rebuild batch with updated status + updated_at
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for col_idx in 0..batch.num_columns() {
            if col_idx == items_col::STATUS {
                let statuses = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("status column");
                let mut new_statuses: Vec<String> = (0..batch.num_rows())
                    .map(|i| statuses.value(i).to_string())
                    .collect();
                new_statuses[row_idx] = new_status.to_string();
                let refs: Vec<&str> = new_statuses.iter().map(|s| s.as_str()).collect();
                columns.push(Arc::new(StringArray::from(refs)));
            } else if col_idx == items_col::UPDATED_AT {
                let ts = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<TimestampMillisecondArray>()
                    .expect("updated_at column");
                let mut new_ts: Vec<Option<i64>> = (0..batch.num_rows())
                    .map(|i| {
                        if ts.is_null(i) {
                            None
                        } else {
                            Some(ts.value(i))
                        }
                    })
                    .collect();
                new_ts[row_idx] = Some(now_ms);
                columns.push(Arc::new(
                    TimestampMillisecondArray::from(new_ts).with_timezone("UTC"),
                ));
            } else {
                columns.push(batch.column(col_idx).clone());
            }
        }

        let new_batch = RecordBatch::try_new(self.items_schema.clone(), columns)?;
        self.items_batches[batch_idx] = new_batch;

        // Record in runs table
        self.record_run(id, Some(&old_status), new_status, agent, forced, reason)?;

        Ok(old_status)
    }

    /// Update an item's assignee.
    pub fn update_assignee(&mut self, id: &str, assignee: Option<&str>) -> Result<()> {
        let (batch_idx, row_idx, _) = self.find_item_mut(id)?;
        let batch = &self.items_batches[batch_idx];

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for col_idx in 0..batch.num_columns() {
            if col_idx == items_col::ASSIGNEE {
                let assignees = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("assignee column");
                let mut new_assignees: Vec<Option<String>> = (0..batch.num_rows())
                    .map(|i| {
                        if assignees.is_null(i) {
                            None
                        } else {
                            Some(assignees.value(i).to_string())
                        }
                    })
                    .collect();
                new_assignees[row_idx] = assignee.map(|s| s.to_string());
                let refs: Vec<Option<&str>> = new_assignees.iter().map(|s| s.as_deref()).collect();
                columns.push(Arc::new(StringArray::from(refs)));
            } else {
                columns.push(batch.column(col_idx).clone());
            }
        }

        let new_batch = RecordBatch::try_new(self.items_schema.clone(), columns)?;
        self.items_batches[batch_idx] = new_batch;
        Ok(())
    }

    /// Update a non-nullable string column (title, priority, etc.).
    pub fn update_string_field(&mut self, id: &str, col_idx: usize, value: &str) -> Result<()> {
        let (batch_idx, row_idx, _) = self.find_item_mut(id)?;
        let batch = &self.items_batches[batch_idx];

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for ci in 0..batch.num_columns() {
            if ci == col_idx {
                let col = batch
                    .column(ci)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("string column for update");
                let mut vals: Vec<String> = (0..batch.num_rows())
                    .map(|i| col.value(i).to_string())
                    .collect();
                vals[row_idx] = value.to_string();
                let refs: Vec<&str> = vals.iter().map(|s| s.as_str()).collect();
                columns.push(Arc::new(StringArray::from(refs)));
            } else {
                columns.push(batch.column(ci).clone());
            }
        }

        let new_batch = RecordBatch::try_new(self.items_schema.clone(), columns)?;
        self.items_batches[batch_idx] = new_batch;
        Ok(())
    }

    /// Update a nullable string column (priority, assignee, body).
    pub fn update_nullable_string_field(
        &mut self,
        id: &str,
        col_idx: usize,
        value: Option<&str>,
    ) -> Result<()> {
        let (batch_idx, row_idx, _) = self.find_item_mut(id)?;
        let batch = &self.items_batches[batch_idx];

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for ci in 0..batch.num_columns() {
            if ci == col_idx {
                let col = batch
                    .column(ci)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("nullable string column for update");
                let mut vals: Vec<Option<String>> = (0..batch.num_rows())
                    .map(|i| {
                        if col.is_null(i) {
                            None
                        } else {
                            Some(col.value(i).to_string())
                        }
                    })
                    .collect();
                vals[row_idx] = value.map(|s| s.to_string());
                let refs: Vec<Option<&str>> = vals.iter().map(|s| s.as_deref()).collect();
                columns.push(Arc::new(StringArray::from(refs)));
            } else {
                columns.push(batch.column(ci).clone());
            }
        }

        let new_batch = RecordBatch::try_new(self.items_schema.clone(), columns)?;
        self.items_batches[batch_idx] = new_batch;
        // Don't touch updated_at here — callers that need it (update_body etc.)
        // go through update_nullable_string_field which may be called twice
        // (e.g., body + body_hash). The public methods call touch_updated_at once.
        Ok(())
    }

    /// Update a list column (tags, related, depends_on).
    pub fn update_list_field(&mut self, id: &str, col_idx: usize, values: &[String]) -> Result<()> {
        let (batch_idx, row_idx, _) = self.find_item_mut(id)?;
        let batch = &self.items_batches[batch_idx];

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for ci in 0..batch.num_columns() {
            if ci == col_idx {
                let list_col = batch
                    .column(ci)
                    .as_any()
                    .downcast_ref::<ListArray>()
                    .expect("list column for update");

                // Rebuild the list array with the updated row
                let mut builder =
                    arrow::array::ListBuilder::new(arrow::array::StringBuilder::new());
                for i in 0..batch.num_rows() {
                    if i == row_idx {
                        for v in values {
                            builder.values().append_value(v);
                        }
                        builder.append(true);
                    } else if list_col.is_null(i) {
                        builder.append(false);
                    } else {
                        let list_value = list_col.value(i);
                        let old_values = list_value
                            .as_any()
                            .downcast_ref::<StringArray>()
                            .expect("string list values");
                        for j in 0..old_values.len() {
                            if !old_values.is_null(j) {
                                builder.values().append_value(old_values.value(j));
                            }
                        }
                        builder.append(true);
                    }
                }
                columns.push(Arc::new(builder.finish()));
            } else {
                columns.push(batch.column(ci).clone());
            }
        }

        let new_batch = RecordBatch::try_new(self.items_schema.clone(), columns)?;
        self.items_batches[batch_idx] = new_batch;
        Ok(())
    }

    /// Update an item's title.
    pub fn update_title(&mut self, id: &str, title: &str) -> Result<()> {
        self.update_string_field(id, items_col::TITLE, title)?;
        self.touch_updated_at(id)
    }

    /// Update an item's priority.
    pub fn update_priority(&mut self, id: &str, priority: Option<&str>) -> Result<()> {
        self.update_nullable_string_field(id, items_col::PRIORITY, priority)?;
        self.touch_updated_at(id)
    }

    /// Update an item's body content (also recalculates body_hash).
    pub fn update_body(&mut self, id: &str, body: Option<&str>) -> Result<()> {
        self.update_nullable_string_field(id, items_col::BODY, body)?;
        let hash = body.map(|b| {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(b.as_bytes());
            format!("{:x}", hasher.finalize())
        });
        self.update_nullable_string_field(id, items_col::BODY_HASH, hash.as_deref())?;
        self.touch_updated_at(id)
    }

    /// Update an item's tags (replaces entire tag list).
    pub fn update_tags(&mut self, id: &str, tags: &[String]) -> Result<()> {
        self.update_list_field(id, items_col::TAGS, tags)?;
        self.touch_updated_at(id)
    }

    /// Update an item's related items (replaces entire list).
    pub fn update_related(&mut self, id: &str, related: &[String]) -> Result<()> {
        self.update_list_field(id, items_col::RELATED, related)?;
        self.touch_updated_at(id)
    }

    /// Update an item's depends_on list (replaces entire list).
    pub fn update_depends_on(&mut self, id: &str, depends_on: &[String]) -> Result<()> {
        self.update_list_field(id, items_col::DEPENDS_ON, depends_on)?;
        self.touch_updated_at(id)
    }

    /// Update an item's resolution (completed, superseded, wont_do, duplicate, obsolete, merged).
    pub fn update_resolution(&mut self, id: &str, resolution: Option<&str>) -> Result<()> {
        self.update_nullable_string_field(id, items_col::RESOLUTION, resolution)?;
        self.touch_updated_at(id)
    }

    /// Update an item's closed_by provenance URI (e.g., PROP-2025, PR URL).
    pub fn update_closed_by(&mut self, id: &str, closed_by: Option<&str>) -> Result<()> {
        self.update_nullable_string_field(id, items_col::CLOSED_BY, closed_by)?;
        self.touch_updated_at(id)
    }

    /// Generic nullable Int32 column update. Pattern matches
    /// `update_nullable_string_field` but for the Int32Array column type.
    /// Does NOT touch `updated_at` — public callers wrap this and bump it.
    pub fn update_nullable_int32_field(
        &mut self,
        id: &str,
        col_idx: usize,
        value: Option<i32>,
    ) -> Result<()> {
        let (batch_idx, row_idx, _) = self.find_item_mut(id)?;
        let batch = &self.items_batches[batch_idx];

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for ci in 0..batch.num_columns() {
            if ci == col_idx {
                let col = batch
                    .column(ci)
                    .as_any()
                    .downcast_ref::<Int32Array>()
                    .expect("nullable int32 column for update");
                let mut vals: Vec<Option<i32>> = (0..batch.num_rows())
                    .map(|i| {
                        if col.is_null(i) {
                            None
                        } else {
                            Some(col.value(i))
                        }
                    })
                    .collect();
                vals[row_idx] = value;
                columns.push(Arc::new(Int32Array::from(vals)));
            } else {
                columns.push(batch.column(ci).clone());
            }
        }

        let new_batch = RecordBatch::try_new(self.items_schema.clone(), columns)?;
        self.items_batches[batch_idx] = new_batch;
        Ok(())
    }

    /// Set or clear an item's manual rank (Captain priority ordering;
    /// lower = higher priority, `None` = unranked). Writes to the
    /// existing `priority_rank` Int32 column.
    pub fn update_rank(&mut self, id: &str, rank: Option<i32>) -> Result<()> {
        self.update_nullable_int32_field(id, items_col::PRIORITY_RANK, rank)?;
        self.touch_updated_at(id)
    }

    /// Logical delete an item.
    pub fn delete_item(&mut self, id: &str) -> Result<()> {
        let (batch_idx, row_idx, _) = self.find_item_mut(id)?;
        let batch = &self.items_batches[batch_idx];

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for col_idx in 0..batch.num_columns() {
            if col_idx == items_col::DELETED {
                let deleted = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<BooleanArray>()
                    .expect("deleted column");
                let mut new_deleted: Vec<bool> =
                    (0..batch.num_rows()).map(|i| deleted.value(i)).collect();
                new_deleted[row_idx] = true;
                columns.push(Arc::new(BooleanArray::from(new_deleted)));
            } else {
                columns.push(batch.column(col_idx).clone());
            }
        }

        let new_batch = RecordBatch::try_new(self.items_schema.clone(), columns)?;
        self.items_batches[batch_idx] = new_batch;
        Ok(())
    }

    /// Query items with optional filters.
    pub fn query_items(
        &self,
        status: Option<&str>,
        item_type: Option<&str>,
        board: Option<&str>,
        assignee: Option<&str>,
    ) -> Vec<RecordBatch> {
        let mut results = Vec::new();

        for batch in &self.items_batches {
            let statuses = batch
                .column(items_col::STATUS)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("status");
            let types = batch
                .column(items_col::ITEM_TYPE)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("type");
            let boards = batch
                .column(items_col::BOARD)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("board");
            let assignees = batch
                .column(items_col::ASSIGNEE)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("assignee");
            let deleted = batch
                .column(items_col::DELETED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("deleted");

            let mut matching_rows = Vec::new();
            for i in 0..batch.num_rows() {
                if deleted.value(i) {
                    continue;
                }
                if let Some(s) = status
                    && statuses.value(i) != s
                {
                    continue;
                }
                if let Some(t) = item_type
                    && types.value(i) != t
                {
                    continue;
                }
                if let Some(b) = board
                    && boards.value(i) != b
                {
                    continue;
                }
                if let Some(a) = assignee
                    && (assignees.is_null(i) || assignees.value(i) != a)
                {
                    continue;
                }
                matching_rows.push(i);
            }

            // Build filtered batch from matching rows
            for &row in &matching_rows {
                results.push(batch.slice(row, 1));
            }
        }

        results
    }

    /// Count items at a given status (excluding deleted and WIP-exempt types).
    pub fn count_at_status(&self, status: &str, exempt_types: &[&str]) -> u32 {
        let mut count = 0u32;
        for batch in &self.items_batches {
            let statuses = batch
                .column(items_col::STATUS)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("status");
            let types = batch
                .column(items_col::ITEM_TYPE)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("type");
            let deleted = batch
                .column(items_col::DELETED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("deleted");

            for i in 0..batch.num_rows() {
                if deleted.value(i) {
                    continue;
                }
                if statuses.value(i) != status {
                    continue;
                }
                let item_type = types.value(i);
                if exempt_types
                    .iter()
                    .any(|&t| t.eq_ignore_ascii_case(item_type))
                {
                    continue;
                }
                count += 1;
            }
        }
        count
    }

    // --- Internal helpers ---

    /// Find an item by ID, returning (batch_index, row_index, current_status).
    fn find_item_mut(&self, id: &str) -> Result<(usize, usize, String)> {
        for (batch_idx, batch) in self.items_batches.iter().enumerate() {
            let ids = batch
                .column(items_col::ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("id column");
            let deleted = batch
                .column(items_col::DELETED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("deleted column");
            let statuses = batch
                .column(items_col::STATUS)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("status column");

            for i in 0..batch.num_rows() {
                if ids.value(i) == id && !deleted.value(i) {
                    return Ok((batch_idx, i, statuses.value(i).to_string()));
                }
            }
        }
        Err(CrudError::NotFound(id.to_string()))
    }

    /// Add a comment to an item. Returns the allocated comment ID.
    ///
    /// Writes to the CommentsTable (not runs). Also sets `updated_at` on the item.
    pub fn add_comment(
        &mut self,
        item_id: &str,
        text: &str,
        agent: Option<&str>,
    ) -> Result<String> {
        // Verify item exists
        let _ = self.get_item(item_id)?;

        let author = agent.unwrap_or("unknown");
        let schema = self.comments_schema.clone();
        let now_ms = chrono::Utc::now().timestamp_millis();

        // Allocate sequential CMT ID
        let seq = self
            .comments_batches
            .iter()
            .map(|b| b.num_rows())
            .sum::<usize>()
            + 1;
        let comment_id = format!("CMT-{}-{:03}", item_id, seq);

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec![comment_id.as_str()])),
                Arc::new(StringArray::from(vec![item_id])),
                Arc::new(StringArray::from(vec![author])),
                Arc::new(StringArray::from(vec![text])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![None::<&str>])), // no parent
                Arc::new(BooleanArray::from(vec![false])),       // not resolved
            ],
        )?;

        self.comments_batches.push(batch);
        self.touch_updated_at(item_id)?;
        Ok(comment_id)
    }

    /// Set `updated_at` to the current timestamp on an item.
    fn touch_updated_at(&mut self, id: &str) -> Result<()> {
        let (batch_idx, row_idx, _) = self.find_item_mut(id)?;
        let batch = &self.items_batches[batch_idx];
        let now_ms = chrono::Utc::now().timestamp_millis();

        let mut columns: Vec<Arc<dyn Array>> = Vec::new();
        for ci in 0..batch.num_columns() {
            if ci == items_col::UPDATED_AT {
                let ts = batch
                    .column(ci)
                    .as_any()
                    .downcast_ref::<TimestampMillisecondArray>()
                    .expect("updated_at column");
                let mut new_ts: Vec<Option<i64>> = (0..batch.num_rows())
                    .map(|i| {
                        if ts.is_null(i) {
                            None
                        } else {
                            Some(ts.value(i))
                        }
                    })
                    .collect();
                new_ts[row_idx] = Some(now_ms);
                columns.push(Arc::new(
                    TimestampMillisecondArray::from(new_ts).with_timezone("UTC"),
                ));
            } else {
                columns.push(batch.column(ci).clone());
            }
        }

        let new_batch = RecordBatch::try_new(self.items_schema.clone(), columns)?;
        self.items_batches[batch_idx] = new_batch;
        Ok(())
    }

    /// Record a status change in the runs table.
    fn record_run(
        &mut self,
        item_id: &str,
        from_status: Option<&str>,
        to_status: &str,
        agent: Option<&str>,
        forced: bool,
        reason: Option<&str>,
    ) -> Result<()> {
        let run_id = uuid::Uuid::new_v4().to_string();
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            self.runs_schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![run_id.as_str()])),
                Arc::new(StringArray::from(vec![item_id])),
                Arc::new(StringArray::from(vec![from_status])),
                Arc::new(StringArray::from(vec![to_status])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![agent])),
                Arc::new(BooleanArray::from(vec![forced])),
                Arc::new(StringArray::from(vec![reason])),
            ],
        )?;

        self.runs_batches.push(batch);
        Ok(())
    }
}

impl Default for KanbanStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::runs_col;

    fn sample_input(title: &str, item_type: ItemType) -> CreateItemInput {
        CreateItemInput {
            title: title.to_string(),
            item_type,
            priority: Some("high".to_string()),
            assignee: None,
            tags: vec!["v14".to_string()],
            related: vec![],
            depends_on: vec![],
            body: None,
        }
    }

    #[test]
    fn test_create_and_get() {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&sample_input("Test Expedition", ItemType::Expedition))
            .unwrap();
        assert!(id.starts_with("EX-"));

        let item = store.get_item(&id).unwrap();
        assert_eq!(item.num_rows(), 1);

        let title = item
            .column(items_col::TITLE)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0);
        assert_eq!(title, "Test Expedition");
    }

    #[test]
    fn test_sequential_ids() {
        let mut store = KanbanStore::new();
        let id1 = store
            .create_item(&sample_input("First", ItemType::Expedition))
            .unwrap();
        let id2 = store
            .create_item(&sample_input("Second", ItemType::Expedition))
            .unwrap();
        let id3 = store
            .create_item(&sample_input("Third", ItemType::Chore))
            .unwrap();

        assert_eq!(id1, "EX-1300");
        assert_eq!(id2, "EX-1301");
        assert_eq!(id3, "CH-1302"); // Global counter, shared across types
    }

    #[test]
    fn test_update_status() {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&sample_input("Test", ItemType::Expedition))
            .unwrap();

        let old = store
            .update_status(&id, "in_progress", Some("M5"), false, None)
            .unwrap();
        assert_eq!(old, "backlog");

        let item = store.get_item(&id).unwrap();
        let status = item
            .column(items_col::STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0);
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn test_update_assignee() {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&sample_input("Test", ItemType::Expedition))
            .unwrap();

        store.update_assignee(&id, Some("DGX")).unwrap();

        let item = store.get_item(&id).unwrap();
        let assignee = item
            .column(items_col::ASSIGNEE)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0);
        assert_eq!(assignee, "DGX");
    }

    #[test]
    fn test_delete_item() {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&sample_input("Test", ItemType::Expedition))
            .unwrap();
        assert_eq!(store.active_item_count(), 1);

        store.delete_item(&id).unwrap();
        assert_eq!(store.active_item_count(), 0);

        // get_item should fail for deleted items
        assert!(store.get_item(&id).is_err());
    }

    #[test]
    fn test_query_by_status() {
        let mut store = KanbanStore::new();
        store
            .create_item(&sample_input("A", ItemType::Expedition))
            .unwrap();
        let id_b = store
            .create_item(&sample_input("B", ItemType::Expedition))
            .unwrap();
        store
            .create_item(&sample_input("C", ItemType::Chore))
            .unwrap();

        store
            .update_status(&id_b, "in_progress", None, false, None)
            .unwrap();

        let backlog = store.query_items(Some("backlog"), None, None, None);
        assert_eq!(backlog.len(), 2);

        let in_progress = store.query_items(Some("in_progress"), None, None, None);
        assert_eq!(in_progress.len(), 1);
    }

    #[test]
    fn test_query_by_type() {
        let mut store = KanbanStore::new();
        store
            .create_item(&sample_input("E1", ItemType::Expedition))
            .unwrap();
        store
            .create_item(&sample_input("C1", ItemType::Chore))
            .unwrap();
        store
            .create_item(&sample_input("E2", ItemType::Expedition))
            .unwrap();

        let expeditions = store.query_items(None, Some("expedition"), None, None);
        assert_eq!(expeditions.len(), 2);

        let chores = store.query_items(None, Some("chore"), None, None);
        assert_eq!(chores.len(), 1);
    }

    #[test]
    fn test_query_by_board() {
        let mut store = KanbanStore::new();
        store
            .create_item(&sample_input("Dev", ItemType::Expedition))
            .unwrap();
        store
            .create_item(&sample_input("Research", ItemType::Paper))
            .unwrap();

        let dev = store.query_items(None, None, Some("development"), None);
        assert_eq!(dev.len(), 1);

        let research = store.query_items(None, None, Some("research"), None);
        assert_eq!(research.len(), 1);
    }

    #[test]
    fn test_count_at_status() {
        let mut store = KanbanStore::new();
        store
            .create_item(&sample_input("E1", ItemType::Expedition))
            .unwrap();
        let id2 = store
            .create_item(&sample_input("E2", ItemType::Expedition))
            .unwrap();
        store
            .create_item(&sample_input("V1", ItemType::Voyage))
            .unwrap();

        store
            .update_status(&id2, "in_progress", None, false, None)
            .unwrap();

        // 2 at backlog (E1 + V1), but V1 is exempt
        assert_eq!(store.count_at_status("backlog", &["voyage"]), 1);
        assert_eq!(store.count_at_status("backlog", &[]), 2);
        assert_eq!(store.count_at_status("in_progress", &[]), 1);
    }

    #[test]
    fn test_runs_recorded() {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&sample_input("Test", ItemType::Expedition))
            .unwrap();
        store
            .update_status(&id, "in_progress", Some("M5"), false, None)
            .unwrap();

        // Should have 2 runs: creation + status change
        let total_runs: usize = store.runs_batches().iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_runs, 2);

        // Check the second run
        let last_batch = store.runs_batches().last().unwrap();
        let agents = last_batch
            .column(runs_col::BY_AGENT)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(agents.value(0), "M5");
    }

    #[test]
    fn test_forced_move_recorded() {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&sample_input("Test", ItemType::Expedition))
            .unwrap();
        store
            .update_status(&id, "in_progress", Some("M5"), true, Some("WIP override"))
            .unwrap();

        let last_run = store.runs_batches().last().unwrap();
        let forced = last_run
            .column(runs_col::FORCED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        assert!(forced.value(0));

        let reason = last_run
            .column(runs_col::REASON)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(reason.value(0), "WIP override");
    }

    #[test]
    fn test_not_found() {
        let store = KanbanStore::new();
        assert!(store.get_item("EXP-999").is_err());
    }

    #[test]
    fn test_create_with_body() {
        let mut store = KanbanStore::new();
        let body_text = "## Phase 1\n\nDo the thing.\n\n## Phase 2\n\nDo the other thing.";
        let id = store
            .create_item(&CreateItemInput {
                title: "Body Test".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: Some(body_text.to_string()),
            })
            .unwrap();

        let item = store.get_item(&id).unwrap();

        // Body column should have content
        let bodies = item
            .column(items_col::BODY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(!bodies.is_null(0));
        assert_eq!(bodies.value(0), body_text);

        // Body hash should be populated
        let hashes = item
            .column(items_col::BODY_HASH)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(!hashes.is_null(0));
        assert!(!hashes.value(0).is_empty());
    }

    #[test]
    fn test_create_without_body() {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&sample_input("No Body", ItemType::Expedition))
            .unwrap();

        let item = store.get_item(&id).unwrap();

        // Body and body_hash should be null
        let bodies = item
            .column(items_col::BODY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(bodies.is_null(0));

        let hashes = item
            .column(items_col::BODY_HASH)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(hashes.is_null(0));
    }

    // ── Update + Comment Tests (EXP-1289) ──

    fn update_test_item(store: &mut KanbanStore) -> String {
        store
            .create_item(&CreateItemInput {
                title: "Update Test".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("medium".to_string()),
                assignee: Some("Mini".to_string()),
                tags: vec!["v14".to_string()],
                related: vec!["VOY-100".to_string()],
                depends_on: vec!["EXP-99".to_string()],
                body: Some("Original body".to_string()),
            })
            .expect("create")
    }

    #[test]
    fn test_update_title_changes_value() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store.update_title(&id, "New Title").unwrap();
        let item = store.get_item(&id).unwrap();
        let t = item
            .column(items_col::TITLE)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(t.value(0), "New Title");
    }

    #[test]
    fn test_update_priority_set_and_clear() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store.update_priority(&id, Some("critical")).unwrap();
        let item = store.get_item(&id).unwrap();
        let p = item
            .column(items_col::PRIORITY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(p.value(0), "critical");
        store.update_priority(&id, None).unwrap();
        let item2 = store.get_item(&id).unwrap();
        let p2 = item2
            .column(items_col::PRIORITY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(p2.is_null(0));
    }

    #[test]
    fn test_update_rank_set_and_clear() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);

        // Set rank
        store.update_rank(&id, Some(3)).unwrap();
        let item = store.get_item(&id).unwrap();
        let col = item
            .column(items_col::PRIORITY_RANK)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        assert!(!col.is_null(0));
        assert_eq!(col.value(0), 3);

        // Update to a different value
        store.update_rank(&id, Some(1)).unwrap();
        let item2 = store.get_item(&id).unwrap();
        let col2 = item2
            .column(items_col::PRIORITY_RANK)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        assert_eq!(col2.value(0), 1);

        // Clear
        store.update_rank(&id, None).unwrap();
        let item3 = store.get_item(&id).unwrap();
        let col3 = item3
            .column(items_col::PRIORITY_RANK)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        assert!(col3.is_null(0));
    }

    #[test]
    fn test_update_rank_nonexistent_id_errors() {
        let mut store = KanbanStore::new();
        let err = store.update_rank("DOES-NOT-EXIST", Some(1));
        assert!(err.is_err(), "expected NotFound, got {err:?}");
    }

    #[test]
    fn test_update_rank_bumps_updated_at() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        let before = store
            .get_item(&id)
            .unwrap()
            .column(items_col::UPDATED_AT)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap()
            .value(0);
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.update_rank(&id, Some(1)).unwrap();
        let after = store
            .get_item(&id)
            .unwrap()
            .column(items_col::UPDATED_AT)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap()
            .value(0);
        assert!(after > before, "update_rank must touch updated_at");
    }

    #[test]
    fn test_update_body_recalculates_hash() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store.update_body(&id, Some("New body")).unwrap();
        let item = store.get_item(&id).unwrap();
        let b = item
            .column(items_col::BODY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(b.value(0), "New body");
        let h = item
            .column(items_col::BODY_HASH)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(!h.is_null(0));
        assert_eq!(h.value(0).len(), 64);
    }

    #[test]
    fn test_update_body_none_clears_hash() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store.update_body(&id, None).unwrap();
        let item = store.get_item(&id).unwrap();
        assert!(
            item.column(items_col::BODY)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .is_null(0)
        );
        assert!(
            item.column(items_col::BODY_HASH)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .is_null(0)
        );
    }

    #[test]
    fn test_update_tags_replaces_list() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store
            .update_tags(&id, &["arrow".to_string(), "rust".to_string()])
            .unwrap();
        let item = store.get_item(&id).unwrap();
        let tags = item
            .column(items_col::TAGS)
            .as_any()
            .downcast_ref::<ListArray>()
            .unwrap();
        let vals = tags
            .value(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .clone();
        assert_eq!(vals.len(), 2);
        assert_eq!(vals.value(0), "arrow");
    }

    #[test]
    fn test_update_related_replaces_list() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store.update_related(&id, &["EXP-200".to_string()]).unwrap();
        let item = store.get_item(&id).unwrap();
        let r = item
            .column(items_col::RELATED)
            .as_any()
            .downcast_ref::<ListArray>()
            .unwrap();
        let vals = r
            .value(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .clone();
        assert_eq!(vals.len(), 1);
        assert_eq!(vals.value(0), "EXP-200");
    }

    #[test]
    fn test_update_nonexistent_item_errors() {
        let mut store = KanbanStore::new();
        assert!(store.update_title("FAKE-999", "Nope").is_err());
    }

    #[test]
    fn test_add_comment_creates_comment_entry() {
        use crate::schema::cmt_col;

        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        let comment_id = store
            .add_comment(&id, "Test comment", Some("Mini"))
            .unwrap();

        // Comment should be in comments_batches, NOT runs
        assert!(comment_id.starts_with("CMT-"));
        let comments = store.comments_batches();
        assert!(!comments.is_empty());
        let last = &comments[comments.len() - 1];
        let authors = last
            .column(cmt_col::AUTHOR)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(authors.value(0), "Mini");
        let bodies = last
            .column(cmt_col::BODY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(bodies.value(0), "Test comment");
    }

    #[test]
    fn test_add_comment_sets_updated_at() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        let before = chrono::Utc::now().timestamp_millis();
        store.add_comment(&id, "Comment", Some("Mini")).unwrap();
        let item = store.get_item(&id).unwrap();
        let updated = item
            .column(items_col::UPDATED_AT)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert!(!updated.is_null(0));
        assert!(updated.value(0) >= before);
    }

    #[test]
    fn test_update_body_sets_updated_at() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        let before = chrono::Utc::now().timestamp_millis();
        store.update_body(&id, Some("New body")).unwrap();
        let item = store.get_item(&id).unwrap();
        let updated = item
            .column(items_col::UPDATED_AT)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert!(!updated.is_null(0));
        assert!(updated.value(0) >= before);
    }

    // ── Resolution + ClosedBy Tests (EX-3081) ──

    #[test]
    fn test_resolution_set_and_read() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store.update_resolution(&id, Some("completed")).unwrap();
        let item = store.get_item(&id).unwrap();
        let res = item
            .column(items_col::RESOLUTION)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(res.value(0), "completed");
    }

    #[test]
    fn test_resolution_null_by_default() {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&sample_input("Test", ItemType::Expedition))
            .unwrap();
        let item = store.get_item(&id).unwrap();
        let res = item
            .column(items_col::RESOLUTION)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(res.is_null(0));
    }

    #[test]
    fn test_resolution_clear() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store.update_resolution(&id, Some("wont_do")).unwrap();
        store.update_resolution(&id, None).unwrap();
        let item = store.get_item(&id).unwrap();
        let res = item
            .column(items_col::RESOLUTION)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(res.is_null(0));
    }

    #[test]
    fn test_closed_by_set_and_read() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store.update_closed_by(&id, Some("PROP-2025")).unwrap();
        let item = store.get_item(&id).unwrap();
        let cb = item
            .column(items_col::CLOSED_BY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(cb.value(0), "PROP-2025");
    }

    #[test]
    fn test_closed_by_null_by_default() {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&sample_input("Test", ItemType::Expedition))
            .unwrap();
        let item = store.get_item(&id).unwrap();
        let cb = item
            .column(items_col::CLOSED_BY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(cb.is_null(0));
    }

    #[test]
    fn test_resolution_and_closed_by_together() {
        let mut store = KanbanStore::new();
        let id = update_test_item(&mut store);
        store.update_status(&id, "done", None, false, None).unwrap();
        store.update_resolution(&id, Some("superseded")).unwrap();
        store.update_closed_by(&id, Some("PROP-2099")).unwrap();
        let item = store.get_item(&id).unwrap();
        let res = item
            .column(items_col::RESOLUTION)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let cb = item
            .column(items_col::CLOSED_BY)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(res.value(0), "superseded");
        assert_eq!(cb.value(0), "PROP-2099");
    }
}
