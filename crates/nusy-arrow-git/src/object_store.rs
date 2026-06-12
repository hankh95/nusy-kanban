//! Object Store — the live in-memory Arrow tables from `nusy-arrow-core`.
//!
//! The Object Store IS the ArrowGraphStore. This module provides the
//! git-aware wrapper that connects the live store to commit/checkout/save.

use nusy_arrow_core::ArrowGraphStore;
use std::path::PathBuf;

/// Configuration for the git-aware object store.
#[derive(Debug, Clone)]
pub struct GitConfig {
    /// Directory where Parquet snapshots are stored.
    pub snapshot_dir: PathBuf,
}

impl GitConfig {
    pub fn new(snapshot_dir: impl Into<PathBuf>) -> Self {
        GitConfig {
            snapshot_dir: snapshot_dir.into(),
        }
    }
}

impl Default for GitConfig {
    fn default() -> Self {
        GitConfig {
            snapshot_dir: PathBuf::from(".nusy-arrow/snapshots"),
        }
    }
}

/// The git-aware graph store. Wraps ArrowGraphStore with versioning capabilities.
pub struct GitObjectStore {
    /// The live in-memory graph.
    pub store: ArrowGraphStore,
    /// Configuration.
    pub config: GitConfig,
}

impl GitObjectStore {
    /// Create a new git-aware store with default config.
    pub fn new() -> Self {
        GitObjectStore {
            store: ArrowGraphStore::new(),
            config: GitConfig::default(),
        }
    }

    /// Create with a specific snapshot directory.
    pub fn with_snapshot_dir(dir: impl Into<PathBuf>) -> Self {
        GitObjectStore {
            store: ArrowGraphStore::new(),
            config: GitConfig::new(dir),
        }
    }

    /// Path for a commit's snapshot directory.
    pub fn commit_snapshot_dir(&self, commit_id: &str) -> PathBuf {
        self.config.snapshot_dir.join(commit_id)
    }

    /// Path for a namespace's Parquet file within a commit snapshot.
    pub fn namespace_parquet_path(&self, commit_id: &str, namespace: &str) -> PathBuf {
        self.commit_snapshot_dir(commit_id)
            .join(format!("{namespace}.parquet"))
    }
}

impl Default for GitObjectStore {
    fn default() -> Self {
        Self::new()
    }
}
