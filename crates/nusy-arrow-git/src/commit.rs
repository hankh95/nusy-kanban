//! Commit — snapshot current graph state to Parquet + record in CommitsTable.
//!
//! A commit captures:
//! - All namespace RecordBatches as Parquet files
//! - A CommitsTable row with commit_id, parents, message, author, timestamp

use crate::object_store::GitObjectStore;
use arrow::array::{ArrayRef, RecordBatch, StringArray, TimestampMillisecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use nusy_arrow_core::Namespace;
use nusy_arrow_core::schema::TRIPLES_SCHEMA_VERSION;
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use std::fs;
use std::sync::Arc;

/// Errors from commit operations.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Commit not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, CommitError>;

/// Schema for the Commits table.
pub fn commits_schema() -> Schema {
    Schema::new(vec![
        Field::new("commit_id", DataType::Utf8, false),
        Field::new(
            "parent_ids",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            false,
        ),
        Field::new(
            "timestamp",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new("message", DataType::Utf8, false),
        Field::new("author", DataType::Utf8, false),
    ])
}

/// A commit record.
#[derive(Debug, Clone)]
pub struct Commit {
    pub commit_id: String,
    pub parent_ids: Vec<String>,
    pub timestamp_ms: i64,
    pub message: String,
    pub author: String,
}

/// The commits history table.
pub struct CommitsTable {
    schema: Arc<Schema>,
    commits: Vec<Commit>,
}

impl CommitsTable {
    pub fn new() -> Self {
        CommitsTable {
            schema: Arc::new(commits_schema()),
            commits: Vec::new(),
        }
    }

    /// Append a commit record.
    pub fn append(&mut self, commit: Commit) {
        self.commits.push(commit);
    }

    /// Get a commit by ID.
    pub fn get(&self, commit_id: &str) -> Option<&Commit> {
        self.commits.iter().find(|c| c.commit_id == commit_id)
    }

    /// Get all commits (ordered by insertion = chronological).
    pub fn all(&self) -> &[Commit] {
        &self.commits
    }

    /// Number of commits.
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    /// Convert to Arrow RecordBatch.
    pub fn to_record_batch(&self) -> Result<RecordBatch> {
        let n = self.commits.len();
        if n == 0 {
            return Ok(RecordBatch::new_empty(self.schema.clone()));
        }

        let ids: Vec<&str> = self.commits.iter().map(|c| c.commit_id.as_str()).collect();
        let timestamps: Vec<i64> = self.commits.iter().map(|c| c.timestamp_ms).collect();
        let messages: Vec<&str> = self.commits.iter().map(|c| c.message.as_str()).collect();
        let authors: Vec<&str> = self.commits.iter().map(|c| c.author.as_str()).collect();

        // Build parent_ids as List<Utf8>
        let parent_ids_list = build_parent_ids_list(&self.commits);

        let batch = RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(StringArray::from(ids)),
                parent_ids_list,
                Arc::new(TimestampMillisecondArray::from(timestamps).with_timezone("UTC")),
                Arc::new(StringArray::from(messages)),
                Arc::new(StringArray::from(authors)),
            ],
        )?;
        Ok(batch)
    }
}

impl Default for CommitsTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a ListArray of parent_ids from commits.
fn build_parent_ids_list(commits: &[Commit]) -> ArrayRef {
    use arrow::array::ListBuilder;
    use arrow::array::StringBuilder;

    let mut builder = ListBuilder::new(StringBuilder::new());
    for commit in commits {
        for pid in &commit.parent_ids {
            builder.values().append_value(pid);
        }
        builder.append(true);
    }
    Arc::new(builder.finish())
}

/// Create a commit: snapshot all namespaces to Parquet, record in CommitsTable.
///
/// Returns the new Commit.
pub fn create_commit(
    obj_store: &GitObjectStore,
    commits_table: &mut CommitsTable,
    parent_ids: Vec<String>,
    message: &str,
    author: &str,
) -> Result<Commit> {
    let commit_id = uuid::Uuid::new_v4().to_string();
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Create snapshot directory
    let snap_dir = obj_store.commit_snapshot_dir(&commit_id);
    fs::create_dir_all(&snap_dir)?;

    // Write each namespace to Parquet
    for ns in Namespace::ALL {
        let batches = obj_store.store.get_namespace_batches(ns);
        if batches.is_empty() {
            continue;
        }

        let path = obj_store.namespace_parquet_path(&commit_id, ns.as_str());
        let schema = obj_store.store.schema().clone();
        let file = fs::File::create(&path)?;
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(vec![parquet::file::metadata::KeyValue {
                key: "nusy_schema_version".to_string(),
                value: Some(TRIPLES_SCHEMA_VERSION.to_string()),
            }]))
            .build();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;

        for batch in batches {
            writer.write(batch)?;
        }
        writer.close()?;
    }

    let commit = Commit {
        commit_id,
        parent_ids,
        timestamp_ms: now_ms,
        message: message.to_string(),
        author: author.to_string(),
    };

    commits_table.append(commit.clone());
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusy_arrow_core::{Namespace, Triple, YLayer};

    fn sample_triple(subj: &str) -> Triple {
        Triple {
            subject: subj.to_string(),
            predicate: "rdf:type".to_string(),
            object: "Thing".to_string(),
            graph: None,
            confidence: Some(0.9),
            source_document: None,
            source_chunk_id: None,
            extracted_by: None,
            caused_by: None,
            derived_from: None,
            consolidated_at: None,
            certifiability_class: None,
            object_datatype: None,
        }
    }

    #[test]
    fn test_commit_creates_parquet_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        // Add some triples
        for i in 0..10 {
            obj.store
                .add_triple(
                    &sample_triple(&format!("s{i}")),
                    Namespace::World,
                    YLayer::Semantic,
                )
                .unwrap();
        }

        let commit = create_commit(&obj, &mut commits, vec![], "initial", "DGX").unwrap();

        // Parquet file should exist for world namespace
        let parquet_path = obj.namespace_parquet_path(&commit.commit_id, "world");
        assert!(parquet_path.exists(), "Parquet file should exist");

        // CommitsTable should have one entry
        assert_eq!(commits.len(), 1);
        assert_eq!(commits.get(&commit.commit_id).unwrap().message, "initial");
    }

    #[test]
    fn test_multiple_commits_form_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let mut commits = CommitsTable::new();

        obj.store
            .add_triple(&sample_triple("s1"), Namespace::World, YLayer::Semantic)
            .unwrap();

        let c1 = create_commit(&obj, &mut commits, vec![], "first", "DGX").unwrap();

        obj.store
            .add_triple(&sample_triple("s2"), Namespace::World, YLayer::Semantic)
            .unwrap();

        let c2 = create_commit(
            &obj,
            &mut commits,
            vec![c1.commit_id.clone()],
            "second",
            "DGX",
        )
        .unwrap();

        assert_eq!(commits.len(), 2);
        assert_eq!(c2.parent_ids, vec![c1.commit_id]);
    }

    #[test]
    fn test_commits_table_to_record_batch() {
        let mut table = CommitsTable::new();
        table.append(Commit {
            commit_id: "c1".to_string(),
            parent_ids: vec![],
            timestamp_ms: 1000,
            message: "init".to_string(),
            author: "DGX".to_string(),
        });
        table.append(Commit {
            commit_id: "c2".to_string(),
            parent_ids: vec!["c1".to_string()],
            timestamp_ms: 2000,
            message: "second".to_string(),
            author: "DGX".to_string(),
        });

        let batch = table.to_record_batch().unwrap();
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 5);
    }
}
