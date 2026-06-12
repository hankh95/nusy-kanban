//! Refs — branch and HEAD management.
//!
//! A Refs table maps ref_name → commit_id. HEAD is the currently active branch.
//! Branches are lightweight pointers — just a name and a commit ID.

use arrow::array::{BooleanArray, RecordBatch, StringArray, TimestampMillisecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use std::sync::Arc;

/// Schema for the Refs table.
pub fn refs_schema() -> Schema {
    Schema::new(vec![
        Field::new("ref_name", DataType::Utf8, false),
        Field::new("commit_id", DataType::Utf8, false),
        Field::new("ref_type", DataType::Utf8, false), // "branch" or "tag"
        Field::new("is_head", DataType::Boolean, false),
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
    ])
}

/// A single ref (branch or tag).
#[derive(Debug, Clone)]
pub struct Ref {
    pub ref_name: String,
    pub commit_id: String,
    pub ref_type: RefType,
    pub is_head: bool,
    pub created_at_ms: i64,
}

/// Type of ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefType {
    Branch,
    Tag,
}

impl RefType {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefType::Branch => "branch",
            RefType::Tag => "tag",
        }
    }
}

/// The refs table — manages branches and HEAD.
pub struct RefsTable {
    refs: Vec<Ref>,
}

impl RefsTable {
    /// Create a new refs table with a "main" branch (no commit yet).
    pub fn new() -> Self {
        RefsTable { refs: Vec::new() }
    }

    /// Initialize with a first commit on "main".
    pub fn init_main(&mut self, commit_id: &str) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.refs.push(Ref {
            ref_name: "main".to_string(),
            commit_id: commit_id.to_string(),
            ref_type: RefType::Branch,
            is_head: true,
            created_at_ms: now_ms,
        });
    }

    /// Get the current HEAD ref.
    pub fn head(&self) -> Option<&Ref> {
        self.refs.iter().find(|r| r.is_head)
    }

    /// Get a ref by name.
    pub fn get(&self, name: &str) -> Option<&Ref> {
        self.refs.iter().find(|r| r.ref_name == name)
    }

    /// Get the commit ID for a ref name.
    pub fn resolve(&self, name: &str) -> Option<&str> {
        self.get(name).map(|r| r.commit_id.as_str())
    }

    /// Create a new branch at the given commit.
    pub fn create_branch(&mut self, name: &str, commit_id: &str) -> Result<(), RefsError> {
        if self.get(name).is_some() {
            return Err(RefsError::RefExists(name.to_string()));
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.refs.push(Ref {
            ref_name: name.to_string(),
            commit_id: commit_id.to_string(),
            ref_type: RefType::Branch,
            is_head: false,
            created_at_ms: now_ms,
        });
        Ok(())
    }

    /// Switch HEAD to a different branch.
    pub fn switch_head(&mut self, name: &str) -> Result<(), RefsError> {
        if self.get(name).is_none() {
            return Err(RefsError::RefNotFound(name.to_string()));
        }
        for r in &mut self.refs {
            r.is_head = r.ref_name == name;
        }
        Ok(())
    }

    /// Update a branch to point to a new commit.
    ///
    /// Tags are immutable and cannot be updated — use `create_tag` instead.
    pub fn update_ref(&mut self, name: &str, commit_id: &str) -> Result<(), RefsError> {
        let r = self
            .refs
            .iter_mut()
            .find(|r| r.ref_name == name)
            .ok_or_else(|| RefsError::RefNotFound(name.to_string()))?;
        if r.ref_type == RefType::Tag {
            return Err(RefsError::TagImmutable(name.to_string()));
        }
        r.commit_id = commit_id.to_string();
        Ok(())
    }

    /// Delete a branch by name.
    ///
    /// Cannot delete the HEAD branch. Cannot delete a nonexistent branch.
    /// Deleting a branch does NOT delete the commits it pointed to.
    pub fn delete_branch(&mut self, name: &str) -> Result<(), RefsError> {
        let r = self
            .refs
            .iter()
            .find(|r| r.ref_name == name)
            .ok_or_else(|| RefsError::RefNotFound(name.to_string()))?;

        if r.is_head {
            return Err(RefsError::DeleteHead(name.to_string()));
        }

        if r.ref_type != RefType::Branch {
            return Err(RefsError::NotABranch(name.to_string()));
        }

        self.refs.retain(|r| r.ref_name != name);
        Ok(())
    }

    /// Create an immutable tag pointing to a commit.
    ///
    /// Tags cannot be overwritten or moved once created.
    pub fn create_tag(&mut self, name: &str, commit_id: &str) -> Result<(), RefsError> {
        if self.get(name).is_some() {
            return Err(RefsError::RefExists(name.to_string()));
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.refs.push(Ref {
            ref_name: name.to_string(),
            commit_id: commit_id.to_string(),
            ref_type: RefType::Tag,
            is_head: false,
            created_at_ms: now_ms,
        });
        Ok(())
    }

    /// List all tags.
    pub fn tags(&self) -> Vec<&Ref> {
        self.refs
            .iter()
            .filter(|r| r.ref_type == RefType::Tag)
            .collect()
    }

    /// List all branches.
    pub fn branches(&self) -> Vec<&Ref> {
        self.refs
            .iter()
            .filter(|r| r.ref_type == RefType::Branch)
            .collect()
    }

    /// Convert to Arrow RecordBatch.
    pub fn to_record_batch(&self) -> Result<RecordBatch, arrow::error::ArrowError> {
        let schema = Arc::new(refs_schema());
        if self.refs.is_empty() {
            return Ok(RecordBatch::new_empty(schema));
        }

        let names: Vec<&str> = self.refs.iter().map(|r| r.ref_name.as_str()).collect();
        let commits: Vec<&str> = self.refs.iter().map(|r| r.commit_id.as_str()).collect();
        let types: Vec<&str> = self.refs.iter().map(|r| r.ref_type.as_str()).collect();
        let heads: Vec<bool> = self.refs.iter().map(|r| r.is_head).collect();
        let times: Vec<i64> = self.refs.iter().map(|r| r.created_at_ms).collect();

        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(names)),
                Arc::new(StringArray::from(commits)),
                Arc::new(StringArray::from(types)),
                Arc::new(BooleanArray::from(heads)),
                Arc::new(TimestampMillisecondArray::from(times).with_timezone("UTC")),
            ],
        )
    }
}

impl Default for RefsTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors from refs operations.
#[derive(Debug, thiserror::Error)]
pub enum RefsError {
    #[error("Ref already exists: {0}")]
    RefExists(String),

    #[error("Ref not found: {0}")]
    RefNotFound(String),

    #[error("Cannot delete HEAD branch: {0}")]
    DeleteHead(String),

    #[error("Tags are immutable and cannot be moved: {0}")]
    TagImmutable(String),

    #[error("Ref is not a branch: {0}")]
    NotABranch(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_main_and_head() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");

        let head = refs.head().unwrap();
        assert_eq!(head.ref_name, "main");
        assert_eq!(head.commit_id, "c1");
        assert!(head.is_head);
    }

    #[test]
    fn test_create_branch_and_switch() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.create_branch("feature", "c1").unwrap();

        assert_eq!(refs.branches().len(), 2);

        refs.switch_head("feature").unwrap();
        assert_eq!(refs.head().unwrap().ref_name, "feature");
    }

    #[test]
    fn test_duplicate_branch_fails() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        let result = refs.create_branch("main", "c1");
        assert!(result.is_err());
    }

    #[test]
    fn test_switch_nonexistent_branch_fails() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        let result = refs.switch_head("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_update_ref() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.update_ref("main", "c2").unwrap();
        assert_eq!(refs.resolve("main"), Some("c2"));
    }

    #[test]
    fn test_to_record_batch() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.create_branch("dev", "c1").unwrap();

        let batch = refs.to_record_batch().unwrap();
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 5);
    }

    // --- delete_branch tests ---

    #[test]
    fn test_delete_branch_works() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.create_branch("feature", "c1").unwrap();
        assert_eq!(refs.branches().len(), 2);

        refs.delete_branch("feature").unwrap();
        assert_eq!(refs.branches().len(), 1);
        assert!(refs.get("feature").is_none());
    }

    #[test]
    fn test_delete_head_branch_fails() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        let result = refs.delete_branch("main");
        assert!(result.is_err());
        match result.unwrap_err() {
            RefsError::DeleteHead(name) => assert_eq!(name, "main"),
            other => panic!("Expected DeleteHead, got: {other:?}"),
        }
    }

    #[test]
    fn test_delete_nonexistent_branch_fails() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        let result = refs.delete_branch("ghost");
        assert!(result.is_err());
        match result.unwrap_err() {
            RefsError::RefNotFound(name) => assert_eq!(name, "ghost"),
            other => panic!("Expected RefNotFound, got: {other:?}"),
        }
    }

    // --- tag tests ---

    #[test]
    fn test_create_tag() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.create_tag("v1.0", "c1").unwrap();

        let tags = refs.tags();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].ref_name, "v1.0");
        assert_eq!(tags[0].commit_id, "c1");
        assert_eq!(tags[0].ref_type, RefType::Tag);
    }

    #[test]
    fn test_duplicate_tag_fails() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.create_tag("v1.0", "c1").unwrap();
        let result = refs.create_tag("v1.0", "c2");
        assert!(result.is_err());
    }

    #[test]
    fn test_tag_survives_branch_delete() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.create_branch("feature", "c2").unwrap();
        refs.create_tag("v1.0", "c2").unwrap();

        refs.delete_branch("feature").unwrap();

        // Tag should still resolve
        assert_eq!(refs.resolve("v1.0"), Some("c2"));
        assert_eq!(refs.tags().len(), 1);
    }

    #[test]
    fn test_update_ref_rejects_tag() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.create_tag("v1.0", "c1").unwrap();
        let result = refs.update_ref("v1.0", "c2");
        assert!(result.is_err());
        match result.unwrap_err() {
            RefsError::TagImmutable(name) => assert_eq!(name, "v1.0"),
            other => panic!("Expected TagImmutable, got: {other:?}"),
        }
        // Tag should still point to original commit
        assert_eq!(refs.resolve("v1.0"), Some("c1"));
    }

    #[test]
    fn test_delete_branch_rejects_tag() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.create_tag("v1.0", "c1").unwrap();
        let result = refs.delete_branch("v1.0");
        assert!(result.is_err());
        match result.unwrap_err() {
            RefsError::NotABranch(name) => assert_eq!(name, "v1.0"),
            other => panic!("Expected NotABranch, got: {other:?}"),
        }
    }

    #[test]
    fn test_tags_not_in_branches() {
        let mut refs = RefsTable::new();
        refs.init_main("c1");
        refs.create_tag("v1.0", "c1").unwrap();

        // Tags should not appear in branches()
        assert_eq!(refs.branches().len(), 1);
        assert_eq!(refs.tags().len(), 1);
    }
}
