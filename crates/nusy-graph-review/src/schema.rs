use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use std::sync::Arc;

// ── Proposals table column indices ──────────────────────────────────────────

pub mod proposals_col {
    pub const PROPOSAL_ID: usize = 0;
    pub const SOURCE_BRANCH: usize = 1;
    pub const TARGET_BRANCH: usize = 2;
    pub const NAMESPACE: usize = 3;
    pub const PROPOSAL_TYPE: usize = 4;
    pub const STATUS: usize = 5;
    pub const AUTHOR: usize = 6;
    pub const REVIEWER: usize = 7;
    pub const MERGED_BY: usize = 8;
    pub const TITLE: usize = 9;
    pub const DESCRIPTION: usize = 10;
    pub const CREATED_AT: usize = 11;
    pub const UPDATED_AT: usize = 12;
    pub const MERGED_AT: usize = 13;
    pub const RESOLUTION: usize = 14;
    pub const CLOSED_BY: usize = 15;
}

pub fn proposals_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("proposal_id", DataType::Utf8, false),
        Field::new("source_branch", DataType::Utf8, false),
        Field::new("target_branch", DataType::Utf8, false),
        Field::new("namespace", DataType::Utf8, false),
        Field::new("proposal_type", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("author", DataType::Utf8, false),
        Field::new("reviewer", DataType::Utf8, true),
        Field::new("merged_by", DataType::Utf8, true),
        Field::new("title", DataType::Utf8, false),
        Field::new("description", DataType::Utf8, true),
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new(
            "updated_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new(
            "merged_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true,
        ),
        Field::new("resolution", DataType::Utf8, true),
        Field::new("closed_by", DataType::Utf8, true),
    ]))
}

// ── Review comments table column indices ────────────────────────────────────

pub mod comments_col {
    pub const COMMENT_ID: usize = 0;
    pub const PROPOSAL_ID: usize = 1;
    pub const PARENT_COMMENT_ID: usize = 2;
    pub const REVIEWER: usize = 3;
    pub const BODY: usize = 4;
    pub const LINE_REF: usize = 5;
    pub const CREATED_AT: usize = 6;
    pub const RESOLVED: usize = 7;
}

pub fn comments_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("comment_id", DataType::Utf8, false),
        Field::new("proposal_id", DataType::Utf8, false),
        Field::new("parent_comment_id", DataType::Utf8, true),
        Field::new("reviewer", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("line_ref", DataType::Utf8, true),
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new("resolved", DataType::Boolean, false),
    ]))
}

// ── CI results table column indices ──────────────────────────────────────────

pub mod ci_results_col {
    pub const RUN_ID: usize = 0;
    pub const PROPOSAL_ID: usize = 1;
    pub const STATUS: usize = 2;
    pub const TEST_PASSED: usize = 3;
    pub const TEST_FAILED: usize = 4;
    pub const CLIPPY_WARNINGS: usize = 5;
    pub const FMT_CLEAN: usize = 6;
    pub const DURATION_SECS: usize = 7;
    pub const ERROR_MESSAGE: usize = 8;
    pub const SUMMARY: usize = 9;
    pub const COMPLETED_AT: usize = 10;
}

pub fn ci_results_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("run_id", DataType::Utf8, false),
        Field::new("proposal_id", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("test_passed", DataType::UInt32, false),
        Field::new("test_failed", DataType::UInt32, false),
        Field::new("clippy_warnings", DataType::UInt32, false),
        Field::new("fmt_clean", DataType::Boolean, false),
        Field::new("duration_secs", DataType::Float64, false),
        Field::new("error_message", DataType::Utf8, true),
        Field::new("summary", DataType::Utf8, false),
        Field::new(
            "completed_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
    ]))
}

// ── Diff view column indices ────────────────────────────────────────────────

pub mod diff_col {
    pub const SUBJECT: usize = 0;
    pub const PREDICATE: usize = 1;
    pub const OLD_OBJECT: usize = 2;
    pub const NEW_OBJECT: usize = 3;
    pub const CHANGE_TYPE: usize = 4;
}

pub fn diff_view_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("subject", DataType::Utf8, false),
        Field::new("predicate", DataType::Utf8, false),
        Field::new("old_object", DataType::Utf8, true),
        Field::new("new_object", DataType::Utf8, true),
        Field::new("change_type", DataType::Utf8, false),
    ]))
}
