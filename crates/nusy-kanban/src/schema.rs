//! Arrow schemas for the kanban engine tables.
//!
//! Five tables:
//! - `ItemsTable` — work items (expeditions, chores, papers, etc.)
//! - `RelationsTable` — links between items (implements, spawns, blocks, related_to)
//! - `RunsTable` — status change history (audit trail)
//! - `CommentsTable` — item comments (threaded, resolvable)
//! - `EmbeddingsTable` — semantic embeddings for search

use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use std::sync::Arc;

/// Named column indices for ItemsTable.
pub mod items_col {
    pub const ID: usize = 0;
    pub const TITLE: usize = 1;
    pub const ITEM_TYPE: usize = 2;
    pub const STATUS: usize = 3;
    pub const PRIORITY: usize = 4;
    pub const CREATED: usize = 5;
    pub const ASSIGNEE: usize = 6;
    pub const BOARD: usize = 7;
    pub const TAGS: usize = 8;
    pub const RELATED: usize = 9;
    pub const DEPENDS_ON: usize = 10;
    pub const BODY: usize = 11;
    pub const BODY_HASH: usize = 12;
    pub const DELETED: usize = 13;
    pub const RESOLUTION: usize = 14;
    pub const CLOSED_BY: usize = 15;
    pub const UPDATED_AT: usize = 16;
    pub const PRIORITY_RANK: usize = 17;
}

/// Named column indices for RelationsTable.
pub mod rel_col {
    pub const RELATION_ID: usize = 0;
    pub const SOURCE_ID: usize = 1;
    pub const TARGET_ID: usize = 2;
    pub const PREDICATE: usize = 3;
    pub const CREATED_AT: usize = 4;
    pub const DELETED: usize = 5;
}

/// Named column indices for RunsTable (status history).
pub mod runs_col {
    pub const RUN_ID: usize = 0;
    pub const ITEM_ID: usize = 1;
    pub const FROM_STATUS: usize = 2;
    pub const TO_STATUS: usize = 3;
    pub const TIMESTAMP: usize = 4;
    pub const BY_AGENT: usize = 5;
    pub const FORCED: usize = 6;
    pub const REASON: usize = 7;
}

/// Named column indices for CommentsTable (item comments).
pub mod cmt_col {
    pub const COMMENT_ID: usize = 0;
    pub const ITEM_ID: usize = 1;
    pub const AUTHOR: usize = 2;
    pub const BODY: usize = 3;
    pub const CREATED_AT: usize = 4;
    pub const PARENT_COMMENT_ID: usize = 5;
    pub const RESOLVED: usize = 6;
}

/// Schema for the Items table — all kanban work items across both boards.
pub fn items_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false), // "EXP-1257"
        Field::new("title", DataType::Utf8, false),
        Field::new("item_type", DataType::Utf8, false), // "expedition"
        Field::new("status", DataType::Utf8, false),    // "in_progress"
        Field::new("priority", DataType::Utf8, true),   // "high"
        Field::new(
            "created",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new("assignee", DataType::Utf8, true),
        Field::new("board", DataType::Utf8, false), // "development" or "research"
        Field::new(
            "tags",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            false,
        ),
        Field::new(
            "related",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            false,
        ),
        Field::new(
            "depends_on",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            false,
        ),
        Field::new("body", DataType::Utf8, true),
        Field::new("body_hash", DataType::Utf8, true),
        Field::new("deleted", DataType::Boolean, false),
        Field::new("resolution", DataType::Utf8, true), // completed, superseded, wont_do, duplicate, obsolete, merged
        Field::new("closed_by", DataType::Utf8, true),  // provenance URI (e.g., PROP-2025)
        Field::new(
            "updated_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true, // nullable for backward compat with old Parquet
        ),
        Field::new("priority_rank", DataType::Int32, true), // numeric sort order (1=highest)
    ]))
}

/// Schema for the Relations table — cross-item and cross-board links.
pub fn relations_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("relation_id", DataType::Utf8, false),
        Field::new("source_id", DataType::Utf8, false), // "EXP-1257"
        Field::new("target_id", DataType::Utf8, false), // "VOY-145"
        Field::new("predicate", DataType::Utf8, false), // "implements", "spawns", "blocks"
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new("deleted", DataType::Boolean, false),
    ]))
}

/// Schema for the Runs table — status change audit trail.
pub fn runs_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("run_id", DataType::Utf8, false),
        Field::new("item_id", DataType::Utf8, false), // "EXP-1257"
        Field::new("from_status", DataType::Utf8, true), // null for initial creation
        Field::new("to_status", DataType::Utf8, false), // "in_progress"
        Field::new(
            "timestamp",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new("by_agent", DataType::Utf8, true), // "M5", "DGX", etc.
        Field::new("forced", DataType::Boolean, false),
        Field::new("reason", DataType::Utf8, true), // reason for forced move
    ]))
}

/// Schema for the Comments table — threaded, resolvable item comments.
///
/// Stored separately from items and runs. Legacy comments (runs with
/// to_status="comment") are migrated on load.
pub fn comments_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("comment_id", DataType::Utf8, false), // "CMT-EX-3218-001"
        Field::new("item_id", DataType::Utf8, false),    // parent item
        Field::new("author", DataType::Utf8, false),     // who wrote it
        Field::new("body", DataType::Utf8, false),       // markdown content
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new("parent_comment_id", DataType::Utf8, true), // for threading
        Field::new("resolved", DataType::Boolean, false),      // default false
    ]))
}

/// Named column indices for the Experiment Runs table.
pub mod expr_run_col {
    pub const RUN_ID: usize = 0;
    pub const EXPERIMENT_ID: usize = 1;
    pub const RUN_NUMBER: usize = 2;
    pub const STATUS: usize = 3;
    pub const STARTED_AT: usize = 4;
    pub const COMPLETED_AT: usize = 5;
    pub const RESULTS_JSON: usize = 6;
    pub const AGENT: usize = 7;
}

/// Schema for the Experiment Runs table — timestamped runs of research experiments.
///
/// Each row tracks one execution of an experiment. Multiple runs per experiment
/// are expected (different versions, parameters, or retries).
pub fn experiment_runs_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("run_id", DataType::Utf8, false), // "RUN-EXPR-131.1-001"
        Field::new("experiment_id", DataType::Utf8, false), // "EXPR-131.1"
        Field::new("run_number", DataType::UInt32, false), // Sequential: 1, 2, 3...
        Field::new("status", DataType::Utf8, false), // "running" | "complete" | "failed"
        Field::new(
            "started_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new(
            "completed_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true, // null while running
        ),
        Field::new("results_json", DataType::Utf8, true), // JSON blob with metrics
        Field::new("agent", DataType::Utf8, true),        // who started the run
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{
        BooleanArray, ListBuilder, RecordBatch, StringArray, StringBuilder,
        TimestampMillisecondArray,
    };

    #[test]
    fn test_items_schema_creates_record_batch() {
        let schema = items_schema();
        assert_eq!(schema.fields().len(), 18);
        assert_eq!(schema.field(items_col::ID).name(), "id");
        assert_eq!(schema.field(items_col::TITLE).name(), "title");
        assert_eq!(schema.field(items_col::DELETED).name(), "deleted");

        // Verify we can create a RecordBatch with this schema
        let mut tags_builder = ListBuilder::new(StringBuilder::new());
        tags_builder.values().append_value("v14");
        tags_builder.values().append_value("arrow");
        tags_builder.append(true);

        let mut related_builder = ListBuilder::new(StringBuilder::new());
        related_builder.values().append_value("VOY-145");
        related_builder.append(true);

        let mut depends_builder = ListBuilder::new(StringBuilder::new());
        depends_builder.values().append_value("VOY-142");
        depends_builder.append(true);

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["EXP-1257"])),
                Arc::new(StringArray::from(vec!["Arrow-Kanban Engine"])),
                Arc::new(StringArray::from(vec!["expedition"])),
                Arc::new(StringArray::from(vec!["in_progress"])),
                Arc::new(StringArray::from(vec![Some("critical")])),
                Arc::new(
                    TimestampMillisecondArray::from(vec![1710374400000i64]).with_timezone("UTC"),
                ),
                Arc::new(StringArray::from(vec![Some("M5")])),
                Arc::new(StringArray::from(vec!["development"])),
                Arc::new(tags_builder.finish()),
                Arc::new(related_builder.finish()),
                Arc::new(depends_builder.finish()),
                Arc::new(StringArray::from(vec![None::<&str>])), // body
                Arc::new(StringArray::from(vec![None::<&str>])), // body_hash
                Arc::new(BooleanArray::from(vec![false])),
                Arc::new(StringArray::from(vec![None::<&str>])), // resolution
                Arc::new(StringArray::from(vec![None::<&str>])), // closed_by
                Arc::new(TimestampMillisecondArray::from(vec![None::<i64>]).with_timezone("UTC")), // updated_at
                Arc::new(arrow::array::Int32Array::from(vec![None::<i32>])), // priority_rank
            ],
        )
        .expect("should create items RecordBatch");

        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 18);
    }

    #[test]
    fn test_relations_schema_creates_record_batch() {
        let schema = relations_schema();
        assert_eq!(schema.fields().len(), 6);

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["rel-001"])),
                Arc::new(StringArray::from(vec!["EXP-1257"])),
                Arc::new(StringArray::from(vec!["VOY-145"])),
                Arc::new(StringArray::from(vec!["implements"])),
                Arc::new(
                    TimestampMillisecondArray::from(vec![1710374400000i64]).with_timezone("UTC"),
                ),
                Arc::new(BooleanArray::from(vec![false])),
            ],
        )
        .expect("should create relations RecordBatch");

        assert_eq!(batch.num_rows(), 1);
    }

    #[test]
    fn test_runs_schema_creates_record_batch() {
        let schema = runs_schema();
        assert_eq!(schema.fields().len(), 8);

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["run-001"])),
                Arc::new(StringArray::from(vec!["EXP-1257"])),
                Arc::new(StringArray::from(vec![Some("backlog")])),
                Arc::new(StringArray::from(vec!["in_progress"])),
                Arc::new(
                    TimestampMillisecondArray::from(vec![1710374400000i64]).with_timezone("UTC"),
                ),
                Arc::new(StringArray::from(vec![Some("M5")])),
                Arc::new(BooleanArray::from(vec![false])),
                Arc::new(StringArray::from(vec![None::<&str>])),
            ],
        )
        .expect("should create runs RecordBatch");

        assert_eq!(batch.num_rows(), 1);
    }

    #[test]
    fn test_column_indices_match_schema() {
        let schema = items_schema();
        assert_eq!(schema.field(items_col::ID).name(), "id");
        assert_eq!(schema.field(items_col::ITEM_TYPE).name(), "item_type");
        assert_eq!(schema.field(items_col::STATUS).name(), "status");
        assert_eq!(schema.field(items_col::BOARD).name(), "board");
        assert_eq!(schema.field(items_col::TAGS).name(), "tags");
        assert_eq!(schema.field(items_col::RESOLUTION).name(), "resolution");
        assert_eq!(schema.field(items_col::CLOSED_BY).name(), "closed_by");
        assert_eq!(schema.field(items_col::UPDATED_AT).name(), "updated_at");
        assert_eq!(
            schema.field(items_col::PRIORITY_RANK).name(),
            "priority_rank"
        );

        let rel_schema = relations_schema();
        assert_eq!(rel_schema.field(rel_col::SOURCE_ID).name(), "source_id");
        assert_eq!(rel_schema.field(rel_col::TARGET_ID).name(), "target_id");
        assert_eq!(rel_schema.field(rel_col::PREDICATE).name(), "predicate");

        let runs = runs_schema();
        assert_eq!(runs.field(runs_col::ITEM_ID).name(), "item_id");
        assert_eq!(runs.field(runs_col::FROM_STATUS).name(), "from_status");
        assert_eq!(runs.field(runs_col::TO_STATUS).name(), "to_status");
        assert_eq!(runs.field(runs_col::FORCED).name(), "forced");

        let cmts = comments_schema();
        assert_eq!(cmts.field(cmt_col::COMMENT_ID).name(), "comment_id");
        assert_eq!(cmts.field(cmt_col::ITEM_ID).name(), "item_id");
        assert_eq!(cmts.field(cmt_col::AUTHOR).name(), "author");
        assert_eq!(cmts.field(cmt_col::BODY).name(), "body");
        assert_eq!(cmts.field(cmt_col::CREATED_AT).name(), "created_at");
        assert_eq!(
            cmts.field(cmt_col::PARENT_COMMENT_ID).name(),
            "parent_comment_id"
        );
        assert_eq!(cmts.field(cmt_col::RESOLVED).name(), "resolved");
    }

    #[test]
    fn test_comments_schema_field_count() {
        let schema = comments_schema();
        assert_eq!(schema.fields().len(), 7);
    }
}
