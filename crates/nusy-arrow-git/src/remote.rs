//! # Remote — Push/Pull Graph State Over Any Transport
//!
//! ## GRAPH MINDSET: NO FILES, NO SERIALIZATION TO DISK
//!
//! Everything in this module operates on in-memory Arrow RecordBatches.
//! When we "push" graph state, we convert Arrow tables to Parquet bytes
//! **in memory** and send those bytes over the wire. When we "pull", we
//! receive bytes and load them **directly into Arrow tables**. At no point
//! do we write to the filesystem. The graph store IS the data — Parquet
//! is just the wire encoding, like protobuf but columnar.
//!
//! This works for ANY Arrow graph state: being knowledge, code graph
//! objects, kanban items, research data. The `GitObjectStore` doesn't
//! care what's in the RecordBatches — it versions them all the same way.
//!
//! ## Wire Format
//!
//! A length-prefixed JSON manifest followed by raw Parquet segments.
//! The manifest contains commit history (JSON), ref pointers (JSON),
//! and namespace segment offsets. Transport-agnostic — works over NATS,
//! HTTP, TCP, or even carrier pigeon.

use crate::commit::CommitsTable;
use crate::object_store::GitObjectStore;
use crate::refs::RefsTable;
use crate::save::{deserialize_commits, deserialize_refs, serialize_commits, serialize_refs};
use bytes::Bytes;
use nusy_arrow_core::Namespace;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::io::Cursor;

// Note: Zero file I/O in this module. All serialization is to/from in-memory
// byte buffers. The Parquet format is used for namespace data (efficient columnar
// encoding), and lightweight JSON for commits/refs metadata.

/// Errors from remote operations.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("Parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Invalid snapshot: {0}")]
    InvalidSnapshot(String),
}

/// A serialized snapshot of the full git state, ready for transport.
///
/// Contains all namespace data + commit history + refs as Parquet bytes.
/// Self-contained — can be deserialized without any other context.
pub struct Snapshot {
    /// Per-namespace Parquet bytes. Key = namespace name (e.g., "world").
    pub namespaces: Vec<(String, Vec<u8>)>,
    /// CommitsTable serialized as JSON.
    pub commits_json: String,
    /// RefsTable serialized as JSON.
    pub refs_json: String,
}

/// Serialize the current git state to a transportable Snapshot.
///
/// Purely in-memory — no file I/O. Namespaces are serialized to Parquet
/// bytes in memory. Commits and refs use lightweight JSON serialization.
pub fn snapshot_state(
    obj_store: &GitObjectStore,
    commits_table: &CommitsTable,
    refs_table: &RefsTable,
) -> Result<Snapshot, RemoteError> {
    let mut namespaces = Vec::new();

    for ns in Namespace::ALL {
        let batches = obj_store.store.get_namespace_batches(ns);
        if batches.is_empty() {
            continue;
        }

        let schema = obj_store.store.schema().clone();
        let mut buf = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = ArrowWriter::try_new(cursor, schema, None)?;
            for batch in batches {
                writer.write(batch)?;
            }
            writer.close()?;
        }

        namespaces.push((ns.as_str().to_string(), buf));
    }

    // Serialize commits and refs directly to JSON strings — no file I/O
    let commits_json = serialize_commits(commits_table);
    let refs_json = serialize_refs(refs_table);

    Ok(Snapshot {
        namespaces,
        commits_json,
        refs_json,
    })
}

/// Restore git state from a Snapshot received over the wire.
///
/// Purely in-memory — no file I/O. Clears the local store and loads all
/// namespace data from in-memory Parquet bytes. Commits and refs are
/// deserialized from JSON strings.
pub fn restore_snapshot(
    obj_store: &mut GitObjectStore,
    snapshot: &Snapshot,
) -> Result<(CommitsTable, RefsTable), RemoteError> {
    obj_store.store.clear();

    // Restore each namespace from in-memory Parquet bytes
    for (ns_name, parquet_bytes) in &snapshot.namespaces {
        let ns = Namespace::from_str_loose(ns_name)
            .ok_or_else(|| RemoteError::InvalidSnapshot(format!("Unknown namespace: {ns_name}")))?;

        let bytes = Bytes::from(parquet_bytes.clone());
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes)?.build()?;

        let mut batches = Vec::new();
        for batch_result in reader {
            batches.push(batch_result?);
        }

        obj_store.store.set_namespace_batches(ns, batches);
    }

    // Deserialize commits and refs directly from JSON — no file I/O
    let commits = if snapshot.commits_json.is_empty() {
        CommitsTable::new()
    } else {
        deserialize_commits(&snapshot.commits_json)
    };

    let refs = if snapshot.refs_json.is_empty() {
        RefsTable::new()
    } else {
        deserialize_refs(&snapshot.refs_json)
    };

    Ok((commits, refs))
}

/// Serialize a Snapshot to a flat byte vector for NATS transport.
///
/// Format: JSON manifest (length-prefixed) followed by raw Parquet segments.
/// The manifest contains: commits_json, refs_json, and namespace offsets.
pub fn snapshot_to_bytes(snapshot: &Snapshot) -> Vec<u8> {
    // Build manifest
    let mut ns_entries = Vec::new();
    let mut offset = 0u64;
    for (name, data) in &snapshot.namespaces {
        ns_entries.push(format!(
            "{{\"name\":\"{}\",\"offset\":{},\"len\":{}}}",
            name,
            offset,
            data.len()
        ));
        offset += data.len() as u64;
    }

    let manifest = format!(
        "{{\"commits\":{},\"refs\":{},\"namespaces\":[{}]}}",
        &snapshot.commits_json,
        &snapshot.refs_json,
        ns_entries.join(",")
    );

    let manifest_bytes = manifest.as_bytes();
    let manifest_len = (manifest_bytes.len() as u64).to_le_bytes();

    let mut result = Vec::new();
    result.extend_from_slice(&manifest_len);
    result.extend_from_slice(manifest_bytes);
    for (_, data) in &snapshot.namespaces {
        result.extend_from_slice(data);
    }

    result
}

/// Deserialize a Snapshot from bytes received over NATS.
pub fn bytes_to_snapshot(bytes: &[u8]) -> Result<Snapshot, RemoteError> {
    if bytes.len() < 8 {
        return Err(RemoteError::InvalidSnapshot("Too short".into()));
    }

    let manifest_len = u64::from_le_bytes(bytes[..8].try_into().unwrap()) as usize;
    if bytes.len() < 8 + manifest_len {
        return Err(RemoteError::InvalidSnapshot("Manifest truncated".into()));
    }

    let manifest_str = std::str::from_utf8(&bytes[8..8 + manifest_len])
        .map_err(|e| RemoteError::InvalidSnapshot(format!("Invalid UTF-8: {e}")))?;

    // Minimal JSON parsing for the manifest
    let commits_json = extract_json_value(manifest_str, "commits").unwrap_or_default();
    let refs_json = extract_json_value(manifest_str, "refs").unwrap_or_default();

    // Parse namespace entries
    let data_start = 8 + manifest_len;
    let ns_section = extract_json_value(manifest_str, "namespaces").unwrap_or_default();

    let mut namespaces = Vec::new();
    // Parse each namespace entry: {"name":"world","offset":0,"len":1234}
    for entry in extract_json_objects(&ns_section) {
        let name = extract_json_string_field(&entry, "name").unwrap_or_default();
        let offset = extract_json_number_field(&entry, "offset").unwrap_or(0) as usize;
        let len = extract_json_number_field(&entry, "len").unwrap_or(0) as usize;

        if data_start + offset + len <= bytes.len() {
            let data = bytes[data_start + offset..data_start + offset + len].to_vec();
            namespaces.push((name, data));
        }
    }

    Ok(Snapshot {
        namespaces,
        commits_json,
        refs_json,
    })
}

// --- Minimal JSON helpers (no serde dependency) ---

fn extract_json_value(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":", key);
    let start = json.find(&pattern)? + pattern.len();
    let rest = json[start..].trim_start();

    if rest.starts_with('[') {
        // Array value — find matching ]
        let mut depth = 0;
        let mut end = 0;
        for (i, ch) in rest.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        Some(rest[..end].to_string())
    } else if rest.starts_with('{') {
        // Object value — find matching }
        let mut depth = 0;
        let mut end = 0;
        for (i, ch) in rest.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        Some(rest[..end].to_string())
    } else {
        // Primitive — read until , or }
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

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

fn extract_json_string_field(obj: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = obj.find(&pattern)? + pattern.len();
    let rest = &obj[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_json_number_field(obj: &str, key: &str) -> Option<i64> {
    let pattern = format!("\"{}\":", key);
    let start = obj.find(&pattern)? + pattern.len();
    let rest = obj[start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommitsTable, GitObjectStore, RefsTable, create_commit};
    use nusy_arrow_core::{Namespace, Triple, YLayer};

    fn make_triple(s: &str, p: &str, o: &str) -> Triple {
        Triple {
            subject: s.to_string(),
            predicate: p.to_string(),
            object: o.to_string(),
            graph: None,
            confidence: Some(1.0),
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
    fn test_snapshot_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));
        let mut commits = CommitsTable::new();
        let mut refs = RefsTable::new();

        // Build state
        for i in 0..50 {
            obj.store
                .add_triple(
                    &make_triple(&format!("entity-{i}"), "rdf:type", "Thing"),
                    Namespace::World,
                    YLayer::Semantic,
                )
                .unwrap();
        }

        let c1 = create_commit(&obj, &mut commits, vec![], "init", "Mini").unwrap();
        refs.init_main(&c1.commit_id);

        assert_eq!(obj.store.len(), 50);

        // Snapshot
        let snapshot = snapshot_state(&obj, &commits, &refs).unwrap();
        assert!(!snapshot.namespaces.is_empty());
        assert!(!snapshot.commits_json.is_empty());

        // Serialize to bytes
        let bytes = snapshot_to_bytes(&snapshot);
        assert!(bytes.len() > 100);

        // Deserialize
        let restored_snapshot = bytes_to_snapshot(&bytes).unwrap();
        assert_eq!(
            restored_snapshot.namespaces.len(),
            snapshot.namespaces.len()
        );

        // Restore into fresh store
        let mut obj2 = GitObjectStore::with_snapshot_dir(tmp.path().join("snap2"));
        let (commits2, refs2) = restore_snapshot(&mut obj2, &restored_snapshot).unwrap();

        assert_eq!(obj2.store.len(), 50);
        assert_eq!(commits2.len(), 1);
        assert!(refs2.head().is_some());
    }

    #[test]
    fn test_snapshot_empty_store() {
        let tmp = tempfile::tempdir().unwrap();
        let obj = GitObjectStore::with_snapshot_dir(tmp.path());
        let commits = CommitsTable::new();
        let refs = RefsTable::new();

        let snapshot = snapshot_state(&obj, &commits, &refs).unwrap();
        assert!(snapshot.namespaces.is_empty());

        let bytes = snapshot_to_bytes(&snapshot);
        let restored = bytes_to_snapshot(&bytes).unwrap();
        assert!(restored.namespaces.is_empty());
    }

    #[test]
    fn test_snapshot_multiple_namespaces() {
        let tmp = tempfile::tempdir().unwrap();
        let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));
        let mut commits = CommitsTable::new();
        let refs = RefsTable::new();

        obj.store
            .add_triple(
                &make_triple("w", "r", "1"),
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        obj.store
            .add_triple(
                &make_triple("k", "r", "2"),
                Namespace::Work,
                YLayer::Procedural,
            )
            .unwrap();

        let _c1 = create_commit(&obj, &mut commits, vec![], "multi-ns", "test").unwrap();

        let snapshot = snapshot_state(&obj, &commits, &refs).unwrap();
        assert_eq!(snapshot.namespaces.len(), 2);

        let bytes = snapshot_to_bytes(&snapshot);
        let restored = bytes_to_snapshot(&bytes).unwrap();

        let mut obj2 = GitObjectStore::with_snapshot_dir(tmp.path().join("snap2"));
        let (_, _) = restore_snapshot(&mut obj2, &restored).unwrap();
        assert_eq!(obj2.store.len(), 2);
    }

    #[test]
    fn test_bytes_to_snapshot_invalid() {
        assert!(bytes_to_snapshot(&[]).is_err());
        assert!(bytes_to_snapshot(&[0; 4]).is_err());
    }
}
