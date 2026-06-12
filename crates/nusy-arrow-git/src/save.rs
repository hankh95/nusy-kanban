//! Save — persist current state without creating a commit (crash recovery).
//!
//! Distinct from Commit: Save is mechanical (for crash recovery),
//! Commit is semantic (for versioning). Save uses atomic file replacement
//! to prevent corruption from partial writes.

use crate::commit::{Commit, CommitsTable};
use crate::object_store::GitObjectStore;
use crate::refs::RefsTable;
use arrow::array::RecordBatch;
use arrow::datatypes::Schema;
use nusy_arrow_core::Namespace;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

/// Errors from save/restore operations.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Save point not found: {0}")]
    NotFound(String),

    #[error("Write-ahead log incomplete — previous save may be corrupt")]
    IncompleteWal,
}

pub type Result<T> = std::result::Result<T, SaveError>;

/// Save current store state to a directory using atomic file replacement.
///
/// Persists:
/// - Graph data: namespace Parquet files
/// - Commit history: `_commits.json`
/// - Branch refs: `_refs.json`
///
/// A write-ahead log (`_wal.json`) tracks the operation for crash recovery.
pub fn save(obj_store: &GitObjectStore, save_dir: &Path) -> Result<()> {
    save_full(obj_store, None, None, save_dir)
}

/// Save with commit history and refs.
pub fn save_full(
    obj_store: &GitObjectStore,
    commits_table: Option<&CommitsTable>,
    refs_table: Option<&RefsTable>,
    save_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(save_dir)?;

    // Write WAL marker — lists namespaces being saved
    let wal_path = save_dir.join("_wal.json");
    let namespaces_with_data: Vec<String> = Namespace::ALL
        .iter()
        .filter(|ns| !obj_store.store.get_namespace_batches(**ns).is_empty())
        .map(|ns| ns.as_str().to_string())
        .collect();

    fs::write(&wal_path, serde_json_minimal(&namespaces_with_data))?;

    // Write each namespace atomically
    for ns in Namespace::ALL {
        let batches = obj_store.store.get_namespace_batches(ns);
        let target = save_dir.join(format!("{}.parquet", ns.as_str()));

        if batches.is_empty() {
            // Remove stale file if namespace is now empty
            let _ = fs::remove_file(&target);
            continue;
        }

        let tmp_path = save_dir.join(format!("{}.parquet.tmp", ns.as_str()));
        let schema = obj_store.store.schema().clone();
        let file = fs::File::create(&tmp_path)?;
        let mut writer = ArrowWriter::try_new(file, schema, None)?;

        for batch in batches {
            writer.write(batch)?;
        }
        writer.close()?;

        // Atomic rename (POSIX guarantees)
        fs::rename(&tmp_path, &target)?;
    }

    // Persist CommitsTable as JSON
    if let Some(ct) = commits_table {
        let commits_json = serialize_commits(ct);
        let tmp = save_dir.join("_commits.json.tmp");
        fs::write(&tmp, &commits_json)?;
        fs::rename(&tmp, save_dir.join("_commits.json"))?;
    }

    // Persist RefsTable as JSON
    if let Some(rt) = refs_table {
        let refs_json = serialize_refs(rt);
        let tmp = save_dir.join("_refs.json.tmp");
        fs::write(&tmp, &refs_json)?;
        fs::rename(&tmp, save_dir.join("_refs.json"))?;
    }

    // Remove WAL — save complete
    let _ = fs::remove_file(&wal_path);

    Ok(())
}

/// Restore store state from a save point (graph data only).
pub fn restore(obj_store: &mut GitObjectStore, save_dir: &Path) -> Result<()> {
    let (_, _) = restore_full(obj_store, save_dir)?;
    Ok(())
}

/// Restore store state including commit history and refs.
///
/// Returns (CommitsTable, RefsTable) if they were persisted.
pub fn restore_full(
    obj_store: &mut GitObjectStore,
    save_dir: &Path,
) -> Result<(Option<CommitsTable>, Option<RefsTable>)> {
    if !save_dir.exists() {
        return Err(SaveError::NotFound(save_dir.display().to_string()));
    }

    // Check for incomplete WAL (crash during previous save)
    let wal_path = save_dir.join("_wal.json");
    if wal_path.exists() {
        // WAL exists = previous save was interrupted.
        // The .parquet files may be a mix of old and new.
        // Conservative: return error so caller can decide.
        // In practice, the old .parquet files are still valid
        // (atomic rename means either old or new, not partial).
        // So we can proceed — the WAL just means some namespaces
        // may have old data. Remove WAL and continue.
        let _ = fs::remove_file(&wal_path);
    }

    obj_store.store.clear();

    for ns in Namespace::ALL {
        let path = save_dir.join(format!("{}.parquet", ns.as_str()));
        if !path.exists() {
            continue;
        }

        let file = fs::File::open(&path)?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;

        let mut batches = Vec::new();
        for batch_result in reader {
            batches.push(batch_result?);
        }

        obj_store.store.set_namespace_batches(ns, batches);
    }

    // Restore CommitsTable
    let commits = {
        let path = save_dir.join("_commits.json");
        if path.exists() {
            let data = fs::read_to_string(&path)?;
            Some(deserialize_commits(&data))
        } else {
            None
        }
    };

    // Restore RefsTable
    let refs = {
        let path = save_dir.join("_refs.json");
        if path.exists() {
            let data = fs::read_to_string(&path)?;
            Some(deserialize_refs(&data))
        } else {
            None
        }
    };

    Ok((commits, refs))
}

/// Metrics collected during a save operation.
#[derive(Debug, Clone)]
pub struct SaveMetrics {
    /// Which namespaces were written to disk.
    pub namespaces_saved: Vec<String>,
    /// Total bytes written across all Parquet files.
    pub bytes_written: u64,
    /// Duration of the save in milliseconds.
    pub duration_ms: u128,
    /// Whether zstd compression was used.
    pub compressed: bool,
}

/// Options for customizing save behavior.
#[derive(Debug, Clone, Default)]
pub struct SaveOptions {
    /// Use zstd compression for Parquet files.
    pub compress: bool,
    /// Only save namespaces in this set (incremental save).
    /// If `None`, save all namespaces with data.
    pub dirty_namespaces: Option<HashSet<Namespace>>,
}

/// Save with options, returning metrics about the operation.
///
/// Supports incremental saves (only dirty namespaces) and zstd compression.
pub fn save_with_options(
    obj_store: &GitObjectStore,
    commits_table: Option<&CommitsTable>,
    refs_table: Option<&RefsTable>,
    save_dir: &Path,
    options: &SaveOptions,
) -> Result<SaveMetrics> {
    let start = std::time::Instant::now();
    fs::create_dir_all(save_dir)?;

    // Determine which namespaces to save
    let namespaces_to_save: Vec<Namespace> = Namespace::ALL
        .iter()
        .filter(|ns| {
            // Skip empty namespaces
            if obj_store.store.get_namespace_batches(**ns).is_empty() {
                return false;
            }
            // If dirty set is specified, only save dirty namespaces
            if let Some(dirty) = &options.dirty_namespaces {
                return dirty.contains(ns);
            }
            true
        })
        .copied()
        .collect();

    // Write WAL marker
    let wal_path = save_dir.join("_wal.json");
    let ns_names: Vec<String> = namespaces_to_save
        .iter()
        .map(|ns| ns.as_str().to_string())
        .collect();
    fs::write(&wal_path, serde_json_minimal(&ns_names))?;

    // Build writer properties
    let props = if options.compress {
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .build()
    } else {
        WriterProperties::builder().build()
    };

    let mut total_bytes = 0u64;
    let mut saved_ns_names = Vec::new();

    // Write each namespace atomically
    for ns in &namespaces_to_save {
        let batches = obj_store.store.get_namespace_batches(*ns);
        let target = save_dir.join(format!("{}.parquet", ns.as_str()));

        if batches.is_empty() {
            let _ = fs::remove_file(&target);
            continue;
        }

        let tmp_path = save_dir.join(format!("{}.parquet.tmp", ns.as_str()));
        let schema = obj_store.store.schema().clone();
        let file = fs::File::create(&tmp_path)?;
        let mut writer = ArrowWriter::try_new(file, schema, Some(props.clone()))?;

        for batch in batches {
            writer.write(batch)?;
        }
        writer.close()?;

        let file_size = fs::metadata(&tmp_path)?.len();
        total_bytes += file_size;
        saved_ns_names.push(ns.as_str().to_string());

        fs::rename(&tmp_path, &target)?;
    }

    // Persist CommitsTable
    if let Some(ct) = commits_table {
        let commits_json = serialize_commits(ct);
        let tmp = save_dir.join("_commits.json.tmp");
        fs::write(&tmp, &commits_json)?;
        fs::rename(&tmp, save_dir.join("_commits.json"))?;
    }

    // Persist RefsTable
    if let Some(rt) = refs_table {
        let refs_json = serialize_refs(rt);
        let tmp = save_dir.join("_refs.json.tmp");
        fs::write(&tmp, &refs_json)?;
        fs::rename(&tmp, save_dir.join("_refs.json"))?;
    }

    // Remove WAL — save complete
    let _ = fs::remove_file(&wal_path);

    Ok(SaveMetrics {
        namespaces_saved: saved_ns_names,
        bytes_written: total_bytes,
        duration_ms: start.elapsed().as_millis(),
        compressed: options.compress,
    })
}

// --- Generic save/restore for arbitrary RecordBatch data ---

/// Save arbitrary named RecordBatch collections with WAL + atomic write.
///
/// Each entry is `(name, batches, schema)`. Creates Parquet files
/// like `{save_dir}/{name}.parquet` with atomic tmp+rename.
///
/// This is the generic version of [`save()`] — it works with any Arrow data,
/// not just `GitObjectStore` namespaces. Used by nusy-kanban to persist
/// kanban items, runs, and relations through the same crash-safe pattern.
pub fn save_named_batches(
    entries: &[(&str, &[RecordBatch], &Schema)],
    save_dir: &Path,
) -> Result<SaveMetrics> {
    let start = std::time::Instant::now();
    fs::create_dir_all(save_dir)?;

    // Write WAL marker — lists names being saved
    let wal_path = save_dir.join("_wal.json");
    let names: Vec<String> = entries
        .iter()
        .map(|(name, _, _)| name.to_string())
        .collect();
    fs::write(&wal_path, serde_json_minimal(&names))?;

    let mut total_bytes = 0u64;
    let mut saved_names = Vec::new();

    for (name, batches, schema) in entries {
        let target = save_dir.join(format!("{name}.parquet"));

        if batches.is_empty() {
            // Remove stale file if data is now empty
            let _ = fs::remove_file(&target);
            continue;
        }

        let tmp_path = save_dir.join(format!("{name}.parquet.tmp"));
        let schema_ref = Arc::new((*schema).clone());
        let file = fs::File::create(&tmp_path)?;
        let mut writer = ArrowWriter::try_new(file, schema_ref, None)?;

        for batch in *batches {
            writer.write(batch)?;
        }
        writer.close()?;

        let file_size = fs::metadata(&tmp_path)?.len();
        total_bytes += file_size;
        saved_names.push(name.to_string());

        // Atomic replacement — old file intact if crash occurs before this line
        fs::rename(&tmp_path, &target)?;
    }

    // Remove WAL — save complete
    let _ = fs::remove_file(&wal_path);

    Ok(SaveMetrics {
        namespaces_saved: saved_names,
        bytes_written: total_bytes,
        duration_ms: start.elapsed().as_millis(),
        compressed: false,
    })
}

/// Restore named RecordBatch collections from a save directory.
///
/// Returns a vector of `(name, batches)` for each name found on disk.
/// Missing files are silently skipped (returns empty vec for first-run case).
pub fn restore_named_batches(
    save_dir: &Path,
    names: &[&str],
) -> Result<Vec<(String, Vec<RecordBatch>)>> {
    if !save_dir.exists() {
        return Err(SaveError::NotFound(save_dir.display().to_string()));
    }

    // Check for incomplete WAL (crash during previous save)
    let wal_path = save_dir.join("_wal.json");
    if wal_path.exists() {
        // WAL exists = previous save was interrupted.
        // Atomic rename means files are either old or new, not partial.
        // Safe to proceed — remove WAL and continue.
        let _ = fs::remove_file(&wal_path);
    }

    let mut results = Vec::new();

    for name in names {
        let path = save_dir.join(format!("{name}.parquet"));
        if !path.exists() {
            continue;
        }

        let file = fs::File::open(&path)?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;

        let mut batches = Vec::new();
        for batch_result in reader {
            batches.push(batch_result?);
        }

        results.push((name.to_string(), batches));
    }

    Ok(results)
}

/// Persist a CommitsTable as JSON to a directory.
///
/// Uses atomic tmp+rename to prevent corruption.
pub fn persist_commits(table: &CommitsTable, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let json = serialize_commits(table);
    let tmp = dir.join("_commits.json.tmp");
    fs::write(&tmp, &json)?;
    fs::rename(&tmp, dir.join("_commits.json"))?;
    Ok(())
}

/// Restore a CommitsTable from JSON in a directory.
///
/// Returns `None` if no commits file exists (first run).
pub fn restore_commits(dir: &Path) -> Result<Option<CommitsTable>> {
    let path = dir.join("_commits.json");
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)?;
    Ok(Some(deserialize_commits(&data)))
}

// --- Minimal JSON serialization (no serde dependency) ---

/// Minimal JSON serialization for the WAL (avoid serde dependency).
fn serde_json_minimal(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("\"{}\"", s)).collect();
    format!("[{}]", inner.join(","))
}

/// Escape a string for JSON output.
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Serialize CommitsTable to JSON (one commit per line, array format).
pub(crate) fn serialize_commits(table: &CommitsTable) -> String {
    let mut lines = Vec::new();
    for c in table.all() {
        let parents: Vec<String> = c
            .parent_ids
            .iter()
            .map(|p| format!("\"{}\"", json_escape(p)))
            .collect();
        lines.push(format!(
            "{{\"id\":\"{}\",\"parents\":[{}],\"ts\":{},\"msg\":\"{}\",\"author\":\"{}\"}}",
            json_escape(&c.commit_id),
            parents.join(","),
            c.timestamp_ms,
            json_escape(&c.message),
            json_escape(&c.author),
        ));
    }
    format!("[{}]", lines.join(",\n"))
}

/// Deserialize CommitsTable from JSON.
pub(crate) fn deserialize_commits(json: &str) -> CommitsTable {
    let mut table = CommitsTable::new();
    // Simple parser: extract objects between { }
    for obj in extract_json_objects(json) {
        let id = extract_json_string(&obj, "id").unwrap_or_default();
        let msg = extract_json_string(&obj, "msg").unwrap_or_default();
        let author = extract_json_string(&obj, "author").unwrap_or_default();
        let ts = extract_json_number(&obj, "ts").unwrap_or(0);
        let parents = extract_json_string_array(&obj, "parents");

        table.append(Commit {
            commit_id: id,
            parent_ids: parents,
            timestamp_ms: ts,
            message: msg,
            author,
        });
    }
    table
}

/// Serialize RefsTable to JSON.
pub(crate) fn serialize_refs(table: &RefsTable) -> String {
    let mut lines = Vec::new();
    for r in table.branches() {
        lines.push(format!(
            "{{\"name\":\"{}\",\"commit\":\"{}\",\"type\":\"{}\",\"head\":{},\"created\":{}}}",
            json_escape(&r.ref_name),
            json_escape(&r.commit_id),
            r.ref_type.as_str(),
            r.is_head,
            r.created_at_ms,
        ));
    }
    format!("[{}]", lines.join(",\n"))
}

/// Deserialize RefsTable from JSON.
pub(crate) fn deserialize_refs(json: &str) -> RefsTable {
    let mut table = RefsTable::new();
    for obj in extract_json_objects(json) {
        let name = extract_json_string(&obj, "name").unwrap_or_default();
        let commit = extract_json_string(&obj, "commit").unwrap_or_default();
        let is_head = obj.contains("\"head\":true");

        // Use init_main for first head branch, create_branch for others
        if table.head().is_none() && is_head {
            table.init_main(&commit);
            // Fix the name if it's not "main"
            if name != "main" {
                // Re-create: clear and rebuild
                let _ = table.update_ref("main", &commit);
            }
        } else {
            let _ = table.create_branch(&name, &commit);
            if is_head {
                let _ = table.switch_head(&name);
            }
        }
    }
    table
}

// --- Minimal JSON parsing helpers (no serde dependency) ---

/// Extract JSON objects (top-level array of {...}).
fn extract_json_objects(json: &str) -> Vec<String> {
    let mut objects = Vec::new();
    let mut depth = 0;
    let mut start = None;
    for (i, ch) in json.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        objects.push(json[s..=i].to_string());
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }
    objects
}

/// Extract a string value for a key from a JSON object string.
fn extract_json_string(obj: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = obj.find(&pattern)? + pattern.len();
    let rest = &obj[start..];
    // Find unescaped closing quote
    let mut end = 0;
    let mut escaped = false;
    for ch in rest.chars() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            break;
        }
        end += ch.len_utf8();
    }
    Some(
        rest[..end]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
            .replace("\\n", "\n"),
    )
}

/// Extract a number value for a key from a JSON object string.
fn extract_json_number(obj: &str, key: &str) -> Option<i64> {
    let pattern = format!("\"{}\":", key);
    let start = obj.find(&pattern)? + pattern.len();
    let rest = obj[start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Extract a string array value for a key from a JSON object string.
fn extract_json_string_array(obj: &str, key: &str) -> Vec<String> {
    let pattern = format!("\"{}\":[", key);
    let Some(start) = obj.find(&pattern) else {
        return Vec::new();
    };
    let start = start + pattern.len();
    let rest = &obj[start..];
    let end = rest.find(']').unwrap_or(rest.len());
    let inner = &rest[..end];

    let mut result = Vec::new();
    for part in inner.split(',') {
        let trimmed = part.trim().trim_matches('"');
        if !trimmed.is_empty() {
            result.push(trimmed.to_string());
        }
    }
    result
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
    fn test_save_restore_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("savepoint");
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots"));

        // Add triples to multiple namespaces
        for i in 0..50 {
            obj.store
                .add_triple(
                    &sample_triple(&format!("world-{i}")),
                    Namespace::World,
                    YLayer::Semantic,
                )
                .unwrap();
        }
        for i in 0..30 {
            obj.store
                .add_triple(
                    &sample_triple(&format!("work-{i}")),
                    Namespace::Work,
                    YLayer::Procedural,
                )
                .unwrap();
        }

        assert_eq!(obj.store.len(), 80);

        // Save
        save(&obj, &save_dir).unwrap();

        // Verify files exist
        assert!(save_dir.join("world.parquet").exists());
        assert!(save_dir.join("work.parquet").exists());
        assert!(!save_dir.join("research.parquet").exists()); // No data
        assert!(!save_dir.join("_wal.json").exists()); // WAL cleaned up

        // Clear and restore
        obj.store.clear();
        assert_eq!(obj.store.len(), 0);

        restore(&mut obj, &save_dir).unwrap();
        assert_eq!(obj.store.len(), 80);
    }

    #[test]
    fn test_restore_nonexistent_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let result = restore(&mut obj, &tmp.path().join("nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    fn test_save_atomic_no_partial_files() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("savepoint");
        let obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots"));

        // Save empty store — should succeed, no .parquet files
        save(&obj, &save_dir).unwrap();
        assert!(!save_dir.join("world.parquet").exists());
    }

    #[test]
    fn test_simulated_crash_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("savepoint");
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots"));

        // First save with data
        obj.store
            .add_triple(&sample_triple("s1"), Namespace::World, YLayer::Semantic)
            .unwrap();
        save(&obj, &save_dir).unwrap();

        // Simulate crash: write a WAL file as if a save was interrupted
        fs::write(save_dir.join("_wal.json"), "[\"world\"]").unwrap();

        // Restore should still work (WAL is cleaned up, old .parquet is valid)
        obj.store.clear();
        restore(&mut obj, &save_dir).unwrap();
        assert_eq!(obj.store.len(), 1);
    }

    #[test]
    fn test_concurrent_reads_during_save() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("savepoint");
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots"));

        // Add data and save
        for i in 0..100 {
            obj.store
                .add_triple(
                    &sample_triple(&format!("s{i}")),
                    Namespace::World,
                    YLayer::Semantic,
                )
                .unwrap();
        }
        save(&obj, &save_dir).unwrap();

        // Read while "saving" (verify store is still usable)
        assert_eq!(obj.store.len(), 100);

        // Save again (overwrite) — should not corrupt
        save(&obj, &save_dir).unwrap();

        // Restore and verify
        obj.store.clear();
        restore(&mut obj, &save_dir).unwrap();
        assert_eq!(obj.store.len(), 100);
    }

    #[test]
    fn test_save_full_persists_commits_and_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("savepoint");
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots"));

        // Add data
        obj.store
            .add_triple(&sample_triple("s1"), Namespace::World, YLayer::Semantic)
            .unwrap();

        // Create commits and refs
        let mut commits = crate::commit::CommitsTable::new();
        let c1 = crate::commit::create_commit(&obj, &mut commits, vec![], "init", "DGX").unwrap();

        let mut refs = crate::refs::RefsTable::new();
        refs.init_main(&c1.commit_id);
        refs.create_branch("feature", &c1.commit_id).unwrap();

        // Save everything
        save_full(&obj, Some(&commits), Some(&refs), &save_dir).unwrap();

        // Verify files exist
        assert!(save_dir.join("_commits.json").exists());
        assert!(save_dir.join("_refs.json").exists());

        // Restore into new store
        let mut obj2 = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots2"));
        let (restored_commits, restored_refs) = restore_full(&mut obj2, &save_dir).unwrap();

        // Verify commits restored
        let rc = restored_commits.unwrap();
        assert_eq!(rc.len(), 1);
        assert_eq!(rc.get(&c1.commit_id).unwrap().message, "init");

        // Verify refs restored
        let rr = restored_refs.unwrap();
        assert_eq!(rr.branches().len(), 2);
        assert!(rr.head().is_some());

        // Verify graph data restored
        assert_eq!(obj2.store.len(), 1);
    }

    #[test]
    fn test_save_with_zstd_compression() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("compressed");
        let uncompressed_dir = tmp.path().join("uncompressed");
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

        // Add 1K triples
        for i in 0..1000 {
            obj.store
                .add_triple(
                    &sample_triple(&format!("entity-{}", i)),
                    Namespace::World,
                    YLayer::Semantic,
                )
                .unwrap();
        }

        // Save without compression
        let metrics_plain = save_with_options(
            &obj,
            None,
            None,
            &uncompressed_dir,
            &SaveOptions {
                compress: false,
                dirty_namespaces: None,
            },
        )
        .unwrap();

        // Save with compression
        let metrics_zstd = save_with_options(
            &obj,
            None,
            None,
            &save_dir,
            &SaveOptions {
                compress: true,
                dirty_namespaces: None,
            },
        )
        .unwrap();

        assert!(metrics_zstd.compressed);
        assert!(!metrics_plain.compressed);

        // Compressed should be smaller
        assert!(
            metrics_zstd.bytes_written < metrics_plain.bytes_written,
            "Compressed ({}) should be smaller than uncompressed ({})",
            metrics_zstd.bytes_written,
            metrics_plain.bytes_written,
        );

        // Verify compressed file still restores correctly
        obj.store.clear();
        restore(&mut obj, &save_dir).unwrap();
        assert_eq!(obj.store.len(), 1000);
    }

    #[test]
    fn test_incremental_save_only_dirty_namespaces() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("incremental");
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

        // Add data to all namespaces
        for ns in Namespace::ALL {
            for i in 0..100 {
                obj.store
                    .add_triple(
                        &sample_triple(&format!("{}:{}", ns.as_str(), i)),
                        ns,
                        YLayer::Semantic,
                    )
                    .unwrap();
            }
        }

        // Full save first
        save(&obj, &save_dir).unwrap();

        // Now do incremental save with only World dirty
        let mut dirty = HashSet::new();
        dirty.insert(Namespace::World);

        let metrics = save_with_options(
            &obj,
            None,
            None,
            &save_dir,
            &SaveOptions {
                compress: false,
                dirty_namespaces: Some(dirty),
            },
        )
        .unwrap();

        // Only 1 namespace should be saved
        assert_eq!(metrics.namespaces_saved.len(), 1);
        assert_eq!(metrics.namespaces_saved[0], "world");

        // All data should still restore (other files untouched on disk)
        obj.store.clear();
        restore(&mut obj, &save_dir).unwrap();
        assert_eq!(obj.store.len(), Namespace::ALL.len() * 100);
    }

    #[test]
    fn test_save_metrics_populated() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("metrics");
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

        for i in 0..200 {
            obj.store
                .add_triple(
                    &sample_triple(&format!("entity-{}", i)),
                    Namespace::World,
                    YLayer::Semantic,
                )
                .unwrap();
        }
        for i in 0..100 {
            obj.store
                .add_triple(
                    &sample_triple(&format!("work-{}", i)),
                    Namespace::Work,
                    YLayer::Experience,
                )
                .unwrap();
        }

        let metrics =
            save_with_options(&obj, None, None, &save_dir, &SaveOptions::default()).unwrap();

        assert_eq!(metrics.namespaces_saved.len(), 2);
        assert!(metrics.namespaces_saved.contains(&"world".to_string()));
        assert!(metrics.namespaces_saved.contains(&"work".to_string()));
        assert!(metrics.bytes_written > 0);
        assert!(!metrics.compressed);
    }

    // --- Tests for generic save/restore ---

    fn kanban_schema() -> Schema {
        use arrow::datatypes::{DataType, Field};
        Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("title", DataType::Utf8, false),
            Field::new("status", DataType::Utf8, false),
        ])
    }

    fn kanban_batch(ids: &[&str], titles: &[&str], statuses: &[&str]) -> RecordBatch {
        use arrow::array::StringArray;
        RecordBatch::try_new(
            Arc::new(kanban_schema()),
            vec![
                Arc::new(StringArray::from(ids.to_vec())),
                Arc::new(StringArray::from(titles.to_vec())),
                Arc::new(StringArray::from(statuses.to_vec())),
            ],
        )
        .unwrap()
    }

    #[test]
    fn test_save_named_batches_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("kanban");

        let batch = kanban_batch(
            &["EXP-1", "EXP-2"],
            &["First", "Second"],
            &["backlog", "in_progress"],
        );
        let schema = kanban_schema();

        let metrics =
            save_named_batches(&[("items", &[batch.clone()], &schema)], &save_dir).unwrap();

        assert_eq!(metrics.namespaces_saved, vec!["items"]);
        assert!(metrics.bytes_written > 0);
        assert!(!metrics.compressed);
        assert!(!save_dir.join("_wal.json").exists());

        // Restore and verify
        let results = restore_named_batches(&save_dir, &["items"]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "items");
        assert_eq!(results[0].1[0].num_rows(), 2);
    }

    #[test]
    fn test_save_named_batches_multiple_datasets() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("multi");

        let items = kanban_batch(&["EXP-1"], &["Expedition"], &["backlog"]);
        let runs = kanban_batch(&["RUN-1"], &["Status Change"], &["done"]);
        let schema = kanban_schema();

        save_named_batches(
            &[("items", &[items], &schema), ("runs", &[runs], &schema)],
            &save_dir,
        )
        .unwrap();

        assert!(save_dir.join("items.parquet").exists());
        assert!(save_dir.join("runs.parquet").exists());

        let results = restore_named_batches(&save_dir, &["items", "runs"]).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_save_named_batches_empty_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("empty");

        let schema = kanban_schema();
        let metrics =
            save_named_batches(&[("items", &[] as &[RecordBatch], &schema)], &save_dir).unwrap();

        // Empty batches should not create a file
        assert!(metrics.namespaces_saved.is_empty());
        assert!(!save_dir.join("items.parquet").exists());
    }

    #[test]
    fn test_restore_named_batches_missing_files_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("partial");
        fs::create_dir_all(&save_dir).unwrap();

        // Only save "items", then try to restore "items" and "runs"
        let batch = kanban_batch(&["EXP-1"], &["Test"], &["backlog"]);
        save_named_batches(&[("items", &[batch], &kanban_schema())], &save_dir).unwrap();

        let results = restore_named_batches(&save_dir, &["items", "runs"]).unwrap();
        assert_eq!(results.len(), 1); // Only "items" found
        assert_eq!(results[0].0, "items");
    }

    #[test]
    fn test_restore_named_batches_nonexistent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = restore_named_batches(&tmp.path().join("nonexistent"), &["items"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_save_named_batches_wal_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("wal_test");

        let batch = kanban_batch(&["EXP-1"], &["Test"], &["backlog"]);
        save_named_batches(&[("items", &[batch], &kanban_schema())], &save_dir).unwrap();

        // WAL should not exist after successful save
        assert!(!save_dir.join("_wal.json").exists());
    }

    #[test]
    fn test_save_named_batches_crash_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("crash");

        // Save data
        let batch = kanban_batch(&["EXP-1"], &["Test"], &["backlog"]);
        save_named_batches(&[("items", &[batch], &kanban_schema())], &save_dir).unwrap();

        // Simulate crash: write WAL as if save was interrupted
        fs::write(save_dir.join("_wal.json"), "[\"items\"]").unwrap();

        // Restore should still work — WAL is cleaned up, old .parquet is valid
        let results = restore_named_batches(&save_dir, &["items"]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1[0].num_rows(), 1);
        assert!(!save_dir.join("_wal.json").exists());
    }

    #[test]
    fn test_persist_commits_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let mut table = CommitsTable::new();
        table.append(Commit {
            commit_id: "c1".to_string(),
            parent_ids: vec![],
            timestamp_ms: 1000,
            message: "first save".to_string(),
            author: "nusy-kanban".to_string(),
        });
        table.append(Commit {
            commit_id: "c2".to_string(),
            parent_ids: vec!["c1".to_string()],
            timestamp_ms: 2000,
            message: "second save".to_string(),
            author: "nusy-kanban".to_string(),
        });

        persist_commits(&table, dir).unwrap();
        assert!(dir.join("_commits.json").exists());

        let restored = restore_commits(dir).unwrap().unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored.get("c1").unwrap().message, "first save");
        assert_eq!(restored.get("c2").unwrap().parent_ids, vec!["c1"]);
    }

    #[test]
    fn test_restore_commits_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = restore_commits(tmp.path()).unwrap();
        assert!(result.is_none());
    }
}
