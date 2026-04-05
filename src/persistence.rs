//! Kanban persistence engine — WAL + atomic Parquet save, dogfooding nusy-arrow-git.
//!
//! This module wraps `nusy-kanban::persist` with production-grade features:
//! - **Dirty tracking**: only save when state has changed
//! - **WAL (Write-Ahead Log)**: crash-safe atomic writes (via `nusy-arrow-git::save_named_batches`)
//! - **Graph-native commit history**: queryable audit trail via `nusy-arrow-git::CommitsTable`
//! - **Health metrics**: track save count, last save time, bytes written
//! - **Graceful shutdown**: save before exit
//!
//! This is the being persistence pattern dogfood — the same strategy that
//! V14 beings will use for their ArrowGraphStore.

use crate::crud::KanbanStore;
use crate::persist;
use crate::relations::RelationsStore;
use nusy_arrow_git::commit::{Commit, CommitsTable};
use nusy_arrow_git::save::{persist_commits, restore_commits};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Errors from the persistence engine.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("Persist error: {0}")]
    Persist(#[from] persist::PersistError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Save error: {0}")]
    Save(#[from] nusy_arrow_git::save::SaveError),
}

pub type Result<T> = std::result::Result<T, PersistenceError>;

/// Configuration for the persistence engine.
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// Root directory for kanban data.
    pub root: PathBuf,
    /// Minimum interval between periodic saves (default: 30s).
    pub save_interval: Duration,
    /// Interval between graph-native commit snapshots (default: 5 minutes).
    pub commit_interval: Duration,
    /// Whether to save after every mutation (default: true).
    pub save_on_mutation: bool,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        PersistenceConfig {
            root: PathBuf::from("."),
            save_interval: Duration::from_secs(30),
            commit_interval: Duration::from_secs(300),
            save_on_mutation: true,
        }
    }
}

/// Health metrics for the persistence engine.
#[derive(Debug, Clone)]
pub struct HealthMetrics {
    /// Total number of saves performed since startup.
    pub save_count: u64,
    /// Timestamp of last successful save (millis since epoch).
    pub last_save_at: Option<u64>,
    /// Duration of last save operation.
    pub last_save_duration: Option<Duration>,
    /// Whether there are unsaved changes.
    pub dirty: bool,
    /// Number of mutations since last save.
    pub mutations_since_save: u64,
    /// Total number of graph-native commits.
    pub git_commit_count: u64,
    /// Timestamp when engine started.
    pub started_at: u64,
    /// Number of items in the store.
    pub item_count: usize,
    /// Number of relations.
    pub relation_count: usize,
}

impl HealthMetrics {
    /// Uptime in seconds.
    pub fn uptime_secs(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        (now.saturating_sub(self.started_at)) / 1000
    }
}

/// The persistence engine — wraps KanbanStore + RelationsStore with
/// dirty tracking, atomic saves (via nusy-arrow-git), and graph-native commits.
pub struct PersistenceEngine {
    config: PersistenceConfig,
    dirty: bool,
    mutations_since_save: u64,
    save_count: u64,
    last_save_at: Option<u64>,
    last_save_duration: Option<Duration>,
    last_periodic_save: Instant,
    git_commit_count: u64,
    last_git_backup: Instant,
    mutations_since_git_commit: u64,
    started_at: u64,
    commits_table: CommitsTable,
    last_commit_id: Option<String>,
}

impl PersistenceEngine {
    /// Create a new persistence engine with the given config.
    pub fn new(config: PersistenceConfig) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        PersistenceEngine {
            config,
            dirty: false,
            mutations_since_save: 0,
            save_count: 0,
            last_save_at: None,
            last_save_duration: None,
            last_periodic_save: Instant::now(),
            git_commit_count: 0,
            last_git_backup: Instant::now(),
            mutations_since_git_commit: 0,
            started_at: now_ms,
            commits_table: CommitsTable::new(),
            last_commit_id: None,
        }
    }

    /// Mark state as dirty (a mutation occurred).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.mutations_since_save += 1;
        self.mutations_since_git_commit += 1;
    }

    /// Whether the engine has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Check if a periodic save is due.
    pub fn periodic_save_due(&self) -> bool {
        self.dirty && self.last_periodic_save.elapsed() >= self.config.save_interval
    }

    /// Check if a graph-native commit is due.
    pub fn git_backup_due(&self) -> bool {
        self.mutations_since_git_commit > 0
            && self.last_git_backup.elapsed() >= self.config.commit_interval
    }

    /// Save the kanban state to Parquet atomically.
    ///
    /// Delegates to `persist::save_all()` which uses `nusy-arrow-git::save_named_batches()`
    /// for crash-safe atomic writes with WAL protection.
    pub fn save(&mut self, store: &KanbanStore, relations: &RelationsStore) -> Result<SaveMetrics> {
        if !self.dirty {
            return Ok(SaveMetrics {
                skipped: true,
                ..Default::default()
            });
        }

        let start = Instant::now();

        // Atomic Parquet save via nusy-arrow-git
        persist::save_all(&self.config.root, store, relations)?;

        let duration = start.elapsed();
        let now = self.now_ms();

        self.dirty = false;
        self.mutations_since_save = 0;
        self.save_count += 1;
        self.last_save_at = Some(now);
        self.last_save_duration = Some(duration);
        self.last_periodic_save = Instant::now();

        Ok(SaveMetrics {
            skipped: false,
            duration,
            items_saved: store.active_item_count(),
            relations_saved: relations.active_count(),
            timestamp_ms: now,
        })
    }

    /// Create a graph-native commit for the audit trail.
    ///
    /// Replaces shell `git add` + `git commit` with a CommitsTable entry
    /// persisted as JSON. History is queryable via `commits()`.
    pub fn git_backup(&mut self, item_count: usize) -> Result<GitBackupMetrics> {
        if self.mutations_since_git_commit == 0 {
            return Ok(GitBackupMetrics {
                skipped: true,
                ..Default::default()
            });
        }

        let msg = format!(
            "kanban: auto-save ({} items, {} changes since last commit)",
            item_count, self.mutations_since_git_commit
        );

        // Create graph-native commit (no shell git)
        let commit = Commit {
            commit_id: uuid::Uuid::new_v4().to_string(),
            parent_ids: self.last_commit_id.clone().into_iter().collect(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            message: msg.clone(),
            author: "nusy-kanban".to_string(),
        };

        self.last_commit_id = Some(commit.commit_id.clone());
        self.commits_table.append(commit);

        // Persist commit history as JSON
        let data_dir = persist::data_dir(&self.config.root)?;
        persist_commits(&self.commits_table, &data_dir)?;

        self.git_commit_count += 1;
        self.mutations_since_git_commit = 0;
        self.last_git_backup = Instant::now();

        Ok(GitBackupMetrics {
            skipped: false,
            message: msg,
            commit_count: self.git_commit_count,
        })
    }

    /// Load commit history from disk (call after startup).
    pub fn load_commits(&mut self) -> Result<()> {
        let data_dir = persist::data_dir(&self.config.root)?;
        if let Some(table) = restore_commits(&data_dir)? {
            if let Some(last) = table.all().last() {
                self.last_commit_id = Some(last.commit_id.clone());
            }
            self.git_commit_count = table.len() as u64;
            self.commits_table = table;
        }
        Ok(())
    }

    /// Get the commit history (graph-native audit trail).
    pub fn commits(&self) -> &CommitsTable {
        &self.commits_table
    }

    /// Check for and recover from an incomplete save (WAL present on startup).
    ///
    /// If a WAL file exists, the previous save was interrupted. The Parquet
    /// files may be in an inconsistent state. Since we use atomic file
    /// replacement (write .tmp then rename), the old files are still valid.
    /// Remove the WAL and proceed — the old state is correct.
    pub fn check_wal_recovery(root: &Path) -> Result<bool> {
        let wal_path = persist::data_dir(root)?.join("_wal.json");
        if wal_path.exists() {
            // WAL exists = previous save was interrupted
            // Old Parquet files are still valid (atomic rename guarantees this)
            let _ = std::fs::remove_file(&wal_path);
            Ok(true) // Recovery was needed
        } else {
            Ok(false) // Clean state
        }
    }

    /// Graceful shutdown — save state before exit.
    pub fn shutdown(&mut self, store: &KanbanStore, relations: &RelationsStore) -> Result<()> {
        if self.dirty {
            self.save(store, relations)?;
        }
        Ok(())
    }

    /// Get current health metrics.
    pub fn health(&self, store: &KanbanStore, relations: &RelationsStore) -> HealthMetrics {
        HealthMetrics {
            save_count: self.save_count,
            last_save_at: self.last_save_at,
            last_save_duration: self.last_save_duration,
            dirty: self.dirty,
            mutations_since_save: self.mutations_since_save,
            git_commit_count: self.git_commit_count,
            started_at: self.started_at,
            item_count: store.active_item_count(),
            relation_count: relations.active_count(),
        }
    }

    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

/// Metrics from a save operation.
#[derive(Debug, Default)]
pub struct SaveMetrics {
    /// Whether the save was skipped (not dirty).
    pub skipped: bool,
    /// Duration of the save operation.
    pub duration: Duration,
    /// Number of items saved.
    pub items_saved: usize,
    /// Number of relations saved.
    pub relations_saved: usize,
    /// Timestamp of the save (millis since epoch).
    pub timestamp_ms: u64,
}

/// Metrics from a graph-native commit operation.
#[derive(Debug, Default)]
pub struct GitBackupMetrics {
    /// Whether the backup was skipped.
    pub skipped: bool,
    /// Commit message used.
    pub message: String,
    /// Total graph-native commits since startup.
    pub commit_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crud::CreateItemInput;
    use crate::item_type::ItemType;

    fn test_config(root: &Path) -> PersistenceConfig {
        PersistenceConfig {
            root: root.to_path_buf(),
            save_interval: Duration::from_millis(100),
            commit_interval: Duration::from_secs(300),
            save_on_mutation: true,
        }
    }

    fn create_test_store() -> (KanbanStore, RelationsStore) {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Test Item".into(),
                item_type: ItemType::Expedition,
                priority: Some("high".into()),
                assignee: None,
                tags: vec!["v14".into()],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create item");
        (store, RelationsStore::new())
    }

    #[test]
    fn test_new_engine_is_clean() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = PersistenceEngine::new(test_config(dir.path()));
        assert!(!engine.is_dirty());
        assert!(!engine.periodic_save_due());
        assert!(!engine.git_backup_due());
    }

    #[test]
    fn test_mark_dirty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        engine.mark_dirty();
        assert!(engine.is_dirty());
        assert_eq!(engine.mutations_since_save, 1);
    }

    #[test]
    fn test_save_clears_dirty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        let (store, rels) = create_test_store();

        engine.mark_dirty();
        assert!(engine.is_dirty());

        let metrics = engine.save(&store, &rels).expect("save");
        assert!(!metrics.skipped);
        assert!(!engine.is_dirty());
        assert_eq!(engine.save_count, 1);
        assert!(engine.last_save_at.is_some());
    }

    #[test]
    fn test_save_skips_when_clean() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        let (store, rels) = create_test_store();

        // Save without marking dirty — should skip
        let metrics = engine.save(&store, &rels).expect("save");
        assert!(metrics.skipped);
        assert_eq!(engine.save_count, 0);
    }

    #[test]
    fn test_save_creates_parquet_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        let (store, rels) = create_test_store();

        engine.mark_dirty();
        engine.save(&store, &rels).expect("save");

        // Parquet files should exist
        let data_dir = dir.path().join(".nusy-kanban");
        assert!(data_dir.join("items.parquet").exists());
        // WAL should not exist after successful save
        assert!(!data_dir.join("_wal.json").exists());
    }

    #[test]
    fn test_wal_recovery_clean() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Create data dir so check_wal_recovery can find it
        std::fs::create_dir_all(dir.path().join(".nusy-kanban")).expect("mkdir");
        let recovered = PersistenceEngine::check_wal_recovery(dir.path()).expect("check");
        assert!(!recovered);
    }

    #[test]
    fn test_wal_recovery_with_wal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().join(".nusy-kanban");
        std::fs::create_dir_all(&data_dir).expect("mkdir");
        std::fs::write(data_dir.join("_wal.json"), r#"["items","runs"]"#).expect("write");

        let recovered = PersistenceEngine::check_wal_recovery(dir.path()).expect("check");
        assert!(recovered);

        // WAL should be cleaned up
        assert!(!data_dir.join("_wal.json").exists());
    }

    #[test]
    fn test_save_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        let (store, rels) = create_test_store();

        engine.mark_dirty();
        engine.save(&store, &rels).expect("save");

        // Load and verify
        let loaded = persist::load_store(dir.path()).expect("load");
        assert_eq!(loaded.active_item_count(), 1);
    }

    #[test]
    fn test_periodic_save_due() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(PersistenceConfig {
            root: dir.path().to_path_buf(),
            save_interval: Duration::from_millis(1), // Very short for testing
            ..Default::default()
        });

        engine.mark_dirty();
        // Wait for interval to elapse
        std::thread::sleep(Duration::from_millis(5));
        assert!(engine.periodic_save_due());
    }

    #[test]
    fn test_health_metrics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        let (store, rels) = create_test_store();

        engine.mark_dirty();
        engine.mark_dirty();
        engine.save(&store, &rels).expect("save");
        engine.mark_dirty();

        let health = engine.health(&store, &rels);
        assert_eq!(health.save_count, 1);
        assert!(health.last_save_at.is_some());
        assert!(health.dirty);
        assert_eq!(health.mutations_since_save, 1);
        assert_eq!(health.item_count, 1);
        assert!(health.uptime_secs() < 5); // Test runs fast
    }

    #[test]
    fn test_shutdown_saves_dirty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        let (store, rels) = create_test_store();

        engine.mark_dirty();
        engine.shutdown(&store, &rels).expect("shutdown");

        assert!(!engine.is_dirty());
        assert_eq!(engine.save_count, 1);

        // Verify data persisted
        let loaded = persist::load_store(dir.path()).expect("load");
        assert_eq!(loaded.active_item_count(), 1);
    }

    #[test]
    fn test_shutdown_skips_when_clean() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        let (store, rels) = create_test_store();

        engine.shutdown(&store, &rels).expect("shutdown");
        assert_eq!(engine.save_count, 0);
    }

    #[test]
    fn test_multiple_save_cycles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        let (mut store, rels) = create_test_store();

        // Cycle 1
        engine.mark_dirty();
        engine.save(&store, &rels).expect("save 1");
        assert_eq!(engine.save_count, 1);

        // Add more items
        store
            .create_item(&CreateItemInput {
                title: "Second Item".into(),
                item_type: ItemType::Chore,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        // Cycle 2
        engine.mark_dirty();
        engine.save(&store, &rels).expect("save 2");
        assert_eq!(engine.save_count, 2);

        // Verify latest state
        let loaded = persist::load_store(dir.path()).expect("load");
        assert_eq!(loaded.active_item_count(), 2);
    }

    #[test]
    fn test_crash_recovery_simulation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));
        let (store, rels) = create_test_store();

        // Save successfully first
        engine.mark_dirty();
        engine.save(&store, &rels).expect("save");

        // Simulate crash: create WAL file (as if save was interrupted)
        let data_dir = dir.path().join(".nusy-kanban");
        std::fs::write(
            data_dir.join("_wal.json"),
            r#"["items","runs","relations"]"#,
        )
        .expect("write wal");

        // On "restart": check for WAL recovery
        let recovered = PersistenceEngine::check_wal_recovery(dir.path()).expect("recovery");
        assert!(recovered);

        // Data should still be intact from the last successful save
        let loaded = persist::load_store(dir.path()).expect("load after crash");
        assert_eq!(loaded.active_item_count(), 1);
    }

    #[test]
    fn test_git_backup_creates_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));

        engine.mark_dirty();
        let metrics = engine.git_backup(5).expect("git backup");

        assert!(!metrics.skipped);
        assert_eq!(metrics.commit_count, 1);
        assert!(metrics.message.contains("5 items"));

        // CommitsTable should have one entry
        assert_eq!(engine.commits().len(), 1);
        assert!(engine.last_commit_id.is_some());
    }

    #[test]
    fn test_git_backup_skips_when_clean() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));

        let metrics = engine.git_backup(0).expect("git backup");
        assert!(metrics.skipped);
        assert_eq!(engine.commits().len(), 0);
    }

    #[test]
    fn test_git_backup_chain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));

        // First commit
        engine.mark_dirty();
        engine.git_backup(3).expect("backup 1");

        // Second commit (should have parent)
        engine.mark_dirty();
        engine.git_backup(5).expect("backup 2");

        assert_eq!(engine.commits().len(), 2);
        let commits = engine.commits().all();
        assert!(commits[0].parent_ids.is_empty()); // First has no parent
        assert_eq!(commits[1].parent_ids.len(), 1); // Second has parent
        assert_eq!(commits[1].parent_ids[0], commits[0].commit_id);
    }

    #[test]
    fn test_load_commits_on_restart() {
        let dir = tempfile::tempdir().expect("tempdir");

        // First session: create commits
        {
            let mut engine = PersistenceEngine::new(test_config(dir.path()));
            engine.mark_dirty();
            engine.git_backup(3).expect("backup");
            assert_eq!(engine.commits().len(), 1);
        }

        // Second session: load commits
        {
            let mut engine = PersistenceEngine::new(test_config(dir.path()));
            engine.load_commits().expect("load commits");
            assert_eq!(engine.commits().len(), 1);
            assert!(engine.last_commit_id.is_some());
        }
    }

    #[test]
    fn test_git_backup_persists_to_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = PersistenceEngine::new(test_config(dir.path()));

        engine.mark_dirty();
        engine.git_backup(1).expect("backup");

        // _commits.json should exist in data dir
        let commits_path = dir.path().join(".nusy-kanban/_commits.json");
        assert!(commits_path.exists());
    }
}
