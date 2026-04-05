//! ID allocation — scan items table for max ID per type, allocate next.
//!
//! Protocol for multi-agent safety:
//! 1. Fetch remote (git pull)
//! 2. Scan ItemsTable for max ID of the requested type
//! 3. Allocate next sequential ID
//! 4. Commit and push with rebase-retry on conflict
//!
//! The conflict retry is handled at a higher level (CLI/orchestrator).
//! This module provides the core scan + allocate logic.

use crate::item_type::ItemType;
use crate::schema::items_col;
use arrow::array::{Array, RecordBatch, StringArray};

/// Errors from ID allocation.
#[derive(Debug, thiserror::Error)]
pub enum IdAllocError {
    #[error("Unknown item type: {0}")]
    UnknownType(String),
}

pub type Result<T> = std::result::Result<T, IdAllocError>;

/// Scan batches to find the maximum numeric ID for a given prefix.
///
/// E.g., for prefix "EXP", scans IDs like "EXP-1257" and returns 1257.
/// Returns 0 if no items with that prefix exist.
pub fn max_id_for_prefix(batches: &[RecordBatch], prefix: &str) -> u32 {
    let mut max_id = 0u32;
    let prefix_dash = format!("{}-", prefix);

    for batch in batches {
        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id column should be StringArray");

        for i in 0..ids.len() {
            if ids.is_null(i) {
                continue;
            }
            let id_str = ids.value(i);
            if let Some(num_str) = id_str.strip_prefix(&prefix_dash) {
                // Handle dotted IDs like "EXPR-131.1" — take the integer part
                let num_part = num_str.split('.').next().unwrap_or(num_str);
                if let Ok(num) = num_part.parse::<u32>()
                    && num > max_id
                {
                    max_id = num;
                }
            }
        }
    }

    max_id
}

/// Backward-compatible alias.
pub fn max_id_for_type(batches: &[RecordBatch], prefix: &str) -> u32 {
    max_id_for_prefix(batches, prefix)
}

/// Global counter floor — ensures IDs start at 1300+ (continuous with
/// file-era history where the highest was ~EXP-1294).
const GLOBAL_ID_BASE: u32 = 1299;

/// Find the global maximum numeric ID across ALL item types.
///
/// Scans every known prefix (current + legacy) to find the highest number
/// in use. This ensures a single global counter — no two items of any type
/// can share the same number.
pub fn global_max_id(batches: &[RecordBatch]) -> u32 {
    let mut max_id = 0u32;
    let all_types = ItemType::DEV.iter().chain(ItemType::RESEARCH.iter());

    for item_type in all_types {
        for prefix in item_type.all_prefixes() {
            let type_max = max_id_for_prefix(batches, prefix);
            if type_max > max_id {
                max_id = type_max;
            }
        }
    }

    max_id
}

/// Allocate the next ID for a given item type.
///
/// Uses a SINGLE GLOBAL COUNTER across all types. The number is unique
/// regardless of type — `EX-1305` and `CH-1305` cannot both exist.
///
/// Returns the full ID string (e.g., "EX-1300").
pub fn allocate_id(batches: &[RecordBatch], item_type: ItemType) -> String {
    let prefix = item_type.prefix();
    let next = global_max_id(batches).max(GLOBAL_ID_BASE) + 1;
    format!("{}-{}", prefix, next)
}

/// Allocate the next ID from a type string (case-insensitive).
pub fn allocate_id_from_str(batches: &[RecordBatch], type_str: &str) -> Result<String> {
    let item_type = ItemType::from_str_loose(type_str)
        .ok_or_else(|| IdAllocError::UnknownType(type_str.to_string()))?;
    Ok(allocate_id(batches, item_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::items_schema;
    use arrow::array::{
        BooleanArray, ListBuilder, StringArray, StringBuilder, TimestampMillisecondArray,
    };
    use std::sync::Arc;

    fn make_items_batch(ids: &[&str]) -> RecordBatch {
        let schema = items_schema();
        let n = ids.len();

        let mut tags_builder = ListBuilder::new(StringBuilder::new());
        let mut related_builder = ListBuilder::new(StringBuilder::new());
        let mut depends_builder = ListBuilder::new(StringBuilder::new());
        for _ in 0..n {
            tags_builder.append(true);
            related_builder.append(true);
            depends_builder.append(true);
        }

        let statuses: Vec<&str> = vec!["backlog"; n];
        let types: Vec<&str> = vec!["expedition"; n];
        let titles: Vec<&str> = vec!["test"; n];
        let boards: Vec<&str> = vec!["development"; n];
        let timestamps: Vec<i64> = vec![1710374400000; n];

        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(ids.to_vec())),
                Arc::new(StringArray::from(titles)),
                Arc::new(StringArray::from(types)),
                Arc::new(StringArray::from(statuses)),
                Arc::new(StringArray::from(vec![None::<&str>; n])),
                Arc::new(TimestampMillisecondArray::from(timestamps).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![None::<&str>; n])),
                Arc::new(StringArray::from(boards)),
                Arc::new(tags_builder.finish()),
                Arc::new(related_builder.finish()),
                Arc::new(depends_builder.finish()),
                Arc::new(StringArray::from(vec![None::<&str>; n])), // body
                Arc::new(StringArray::from(vec![None::<&str>; n])), // body_hash
                Arc::new(BooleanArray::from(vec![false; n])),
                Arc::new(StringArray::from(vec![None::<&str>; n])), // resolution
                Arc::new(StringArray::from(vec![None::<&str>; n])), // closed_by
                Arc::new(
                    TimestampMillisecondArray::from(vec![None::<i64>; n]).with_timezone("UTC"),
                ), // updated_at
                Arc::new(arrow::array::Int32Array::from(vec![None::<i32>; n])), // priority_rank
            ],
        )
        .expect("should create batch")
    }

    #[test]
    fn test_max_id_empty() {
        let batches: Vec<RecordBatch> = vec![];
        assert_eq!(max_id_for_type(&batches, "EXP"), 0);
    }

    #[test]
    fn test_max_id_single_batch() {
        let batch = make_items_batch(&["EXP-100", "EXP-200", "EXP-150"]);
        assert_eq!(max_id_for_type(&[batch], "EXP"), 200);
    }

    #[test]
    fn test_max_id_ignores_other_prefixes() {
        let batch = make_items_batch(&["EXP-100", "VOY-500", "CHORE-300"]);
        assert_eq!(max_id_for_type(&[batch.clone()], "EXP"), 100);
        assert_eq!(max_id_for_type(&[batch.clone()], "VOY"), 500);
        assert_eq!(max_id_for_type(&[batch], "CHORE"), 300);
    }

    #[test]
    fn test_max_id_handles_dotted() {
        let batch = make_items_batch(&["EXPR-131", "EXPR-131.1", "EXPR-132"]);
        assert_eq!(max_id_for_type(&[batch], "EXPR"), 132);
    }

    #[test]
    fn test_allocate_id() {
        let batch = make_items_batch(&["EXP-1255", "EXP-1256", "EXP-1257"]);
        let next = allocate_id(&[batch], ItemType::Expedition);
        // max(1257, 3000) + 1 = 3001
        assert_eq!(next, "EX-1300");
    }

    #[test]
    fn test_allocate_id_empty() {
        let next = allocate_id(&[], ItemType::Expedition);
        assert_eq!(next, "EX-1300");
    }

    #[test]
    fn test_allocate_id_from_str() {
        let batch = make_items_batch(&["VOY-142", "VOY-145"]);
        let next = allocate_id_from_str(&[batch], "voyage").unwrap();
        // max(145, 1299) + 1 = 1300
        assert_eq!(next, "VY-1300");
    }

    #[test]
    fn test_allocate_id_unknown_type() {
        assert!(allocate_id_from_str(&[], "nonexistent").is_err());
    }

    #[test]
    fn test_allocate_id_above_base() {
        // Once IDs are above GLOBAL_ID_BASE, they increment normally
        let batch = make_items_batch(&["EXP-3001", "EXP-3002", "EXP-3005"]);
        let next = allocate_id(&[batch], ItemType::Expedition);
        assert_eq!(next, "EX-3006");
    }

    #[test]
    fn test_multiple_batches() {
        let b1 = make_items_batch(&["EXP-100", "EXP-200"]);
        let b2 = make_items_batch(&["EXP-300", "EXP-150"]);
        assert_eq!(max_id_for_type(&[b1, b2], "EXP"), 300);
    }
}
