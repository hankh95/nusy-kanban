//! Backup and restore — snapshot kanban Arrow store to timestamped directories.
//!
//! # Snapshot Contents
//!
//! ```text
//! backup_dir/
//!   snapshot-2026-04-07_055839/
//!     _metadata.json          # server version, last commit, timestamp
//!     items.parquet
//!     runs.parquet
//!     comments.parquet
//!     item_comments.parquet
//!     relations.parquet
//!     proposals.parquet        # only if --pr feature enabled
//!     experiment_runs.parquet
//!     _commits.json
//!   snapshot-2026-04-06_120000/
//!     ...
//!   latest -> snapshot-2026-04-07_055839
//! ```
//!
//! # Backup Config (in .yurtle-kanban/config.yaml)
//!
//! ```yaml
//! backup:
//!   destination: /Volumes/mate-mini/nusy-kanban-backup
//!   format: timestamp  # timestamp=YYYY-MM-DD_HHMMSS, date=YYYY-MM-DD
//!   retention: 10      # keep N snapshots (0=infinite)
//!   schedule: daily   # cron expression or simple alias: hourly, daily, weekly
//! ```
//!
//! # Usage
//!
//! ```bash
//! # Manual backup
//! nusy-kanban backup
//!
//! # List available snapshots
//! nusy-kanban backup --list
//!
//! # Restore from snapshot (requires --force)
//! nusy-kanban restore snapshot-2026-04-07_055839
//! ```
//!
//! EX-4010

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

// ── Config ──────────────────────────────────────────────────────────────────

/// Backup section in .yurtle-kanban/config.yaml.
#[derive(Debug, Clone, Deserialize)]
pub struct BackupConfig {
    /// Backup destination (local path or mount point).
    pub destination: PathBuf,
    /// Snapshot naming format.
    #[serde(default = "default_format")]
    pub format: BackupFormat,
    /// How many snapshots to keep (0 = infinite).
    #[serde(default = "default_retention")]
    pub retention: usize,
    /// Cron expression or simple alias: hourly, daily, weekly.
    #[serde(default = "default_schedule")]
    pub schedule: String,
}

fn default_format() -> BackupFormat {
    BackupFormat::Timestamp
}
fn default_retention() -> usize {
    10
}
fn default_schedule() -> String {
    "daily".to_string()
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BackupFormat {
    /// YYYY-MM-DD_HHMMSS
    Timestamp,
    /// YYYY-MM-DD (hourly backups overwrite within same day)
    Date,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            destination: PathBuf::from("/Volumes/mate-mini/nusy-kanban-backup"),
            format: BackupFormat::Timestamp,
            retention: 0, // infinite — Arrow store is small, keep all snapshots
            schedule: "daily".to_string(),
        }
    }
}

impl std::fmt::Display for BackupFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupFormat::Timestamp => write!(f, "timestamp"),
            BackupFormat::Date => write!(f, "date"),
        }
    }
}

// ── Metadata ────────────────────────────────────────────────────────────────

/// Snapshot metadata — stored as _metadata.json inside each snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// nusy-kanban version string.
    pub version: String,
    /// Last commit ID from _commits.json (if present).
    pub last_commit_id: Option<String>,
    /// Number of commit entries.
    pub commit_count: usize,
}

impl Default for SnapshotMetadata {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotMetadata {
    pub fn new() -> Self {
        let now = chrono::Utc::now();
        let commit_id = Self::last_commit_id();
        let commit_count = Self::count_commits().unwrap_or(0);
        Self {
            created_at: now.to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            last_commit_id: commit_id,
            commit_count,
        }
    }

    /// Read last commit ID from _commits.json, returning None if absent or parseable.
    fn last_commit_id() -> Option<String> {
        let commits_path = Path::new(".nusy-kanban/_commits.json");
        if !commits_path.exists() {
            return None;
        }
        let file = File::open(commits_path).ok()?;
        let reader = BufReader::new(file);
        let commits: Vec<serde_json::Value> = serde_json::from_reader(reader).ok()?;
        commits
            .last()
            .and_then(|v| v.get("id").and_then(|id| id.as_str()))
            .map(|s| s.to_string())
    }

    /// Count total commits in _commits.json.
    fn count_commits() -> Option<usize> {
        let commits_path = Path::new(".nusy-kanban/_commits.json");
        if !commits_path.exists() {
            return Some(0);
        }
        let file = File::open(commits_path).ok()?;
        let reader = BufReader::new(file);
        let commits: Vec<serde_json::Value> = serde_json::from_reader(reader).ok()?;
        Some(commits.len())
    }
}

// ── Snapshot Naming ──────────────────────────────────────────────────────────

/// Generate a snapshot directory name from format + timestamp.
pub fn snapshot_name(format: BackupFormat) -> String {
    let now = chrono::Utc::now();
    match format {
        BackupFormat::Timestamp => now.format("snapshot-%Y-%m-%d_%H%M%S").to_string(),
        BackupFormat::Date => now.format("snapshot-%Y-%m-%d").to_string(),
    }
}

/// Parse a snapshot name back to a chrono NaiveDate (for sorting).
pub fn parse_snapshot_date(name: &str) -> Option<chrono::NaiveDate> {
    // Handles both "snapshot-2026-04-07" and "snapshot-2026-04-07_055839"
    let date_str = name.strip_prefix("snapshot-")?;
    let date_str = date_str.split('_').next()?;
    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()
}

// ── Core Operations ─────────────────────────────────────────────────────────

/// Files to snapshot from .nusy-kanban/
const SNAPSHOT_FILES: &[&str] = &[
    "items.parquet",
    "runs.parquet",
    "comments.parquet",
    "item_comments.parquet",
    "relations.parquet",
    "proposals.parquet",
    "experiment_runs.parquet",
    "_commits.json",
];

/// Files to snapshot without proposals.parquet (used when pr feature is disabled).
const SNAPSHOT_FILES_NO_PR: &[&str] = &[
    "items.parquet",
    "runs.parquet",
    "comments.parquet",
    "item_comments.parquet",
    "relations.parquet",
    "experiment_runs.parquet",
    "_commits.json",
];

/// Errors from backup operations.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("destination not found: {0}")]
    DestinationNotFound(String),

    #[error("snapshot not found: {0}")]
    SnapshotNotFound(String),

    #[error("invalid snapshot name: {0}")]
    InvalidSnapshotName(String),

    #[error("restore requires --force flag")]
    RestoreRequiresForce,

    #[error("cannot restore over live store while server may be writing — stop server first")]
    ServerStillRunning,
}

pub type Result<T> = std::result::Result<T, BackupError>;

/// Create a new snapshot of the current kanban store.
pub fn create_snapshot(config: &BackupConfig, root: &Path) -> Result<PathBuf> {
    // Resolve destination
    let dest = config.destination.canonicalize().unwrap_or_else(|_| {
        // Allow relative or non-canonicalized paths
        config.destination.clone()
    });

    if !dest.exists() {
        return Err(BackupError::DestinationNotFound(dest.display().to_string()));
    }

    // Create snapshot directory
    let name = snapshot_name(config.format);
    let snapshot_dir = dest.join(&name);
    fs::create_dir_all(&snapshot_dir)?;

    // Copy all data files
    let data_dir = root.join(".nusy-kanban");
    let files_to_copy: Vec<&str> = if cfg!(feature = "pr") {
        SNAPSHOT_FILES.to_vec()
    } else {
        SNAPSHOT_FILES_NO_PR.to_vec()
    };

    for filename in files_to_copy {
        let src = data_dir.join(filename);
        if src.exists() {
            let dst = snapshot_dir.join(filename);
            fs::copy(&src, &dst)?;
        }
    }

    // Write metadata
    let metadata = SnapshotMetadata::new();
    let meta_path = snapshot_dir.join("_metadata.json");
    let file = File::create(&meta_path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, &metadata)?;
    writer.flush()?;

    // Update 'latest' symlink
    let latest_link = dest.join("latest");
    if latest_link.exists() || latest_link.is_symlink() {
        fs::remove_file(&latest_link).ok();
    }
    std::os::unix::fs::symlink(&name, &latest_link).ok(); // ignore error on non-Unix

    // Prune old snapshots
    prune_old_snapshots(&dest, config.retention)?;

    Ok(snapshot_dir)
}

/// List all available snapshots in the backup destination.
pub fn list_snapshots(config: &BackupConfig) -> Result<Vec<SnapshotInfo>> {
    let dest = config
        .destination
        .canonicalize()
        .unwrap_or_else(|_| config.destination.clone());

    if !dest.exists() {
        return Err(BackupError::DestinationNotFound(dest.display().to_string()));
    }

    let mut snapshots = Vec::new();

    for entry in fs::read_dir(&dest)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("snapshot-") || entry.file_type()?.is_symlink() {
            continue;
        }
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let meta_path = entry.path().join("_metadata.json");
        let metadata = if meta_path.exists() {
            let file = File::open(&meta_path)?;
            let reader = BufReader::new(file);
            serde_json::from_reader(reader).ok()
        } else {
            None
        };

        let created_at = metadata
            .as_ref()
            .and_then(|m: &SnapshotMetadata| {
                chrono::DateTime::parse_from_rfc3339(&m.created_at)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            })
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string());

        let version = metadata
            .as_ref()
            .map(|m: &SnapshotMetadata| m.version.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let commit_count = metadata
            .as_ref()
            .map(|m: &SnapshotMetadata| m.commit_count)
            .unwrap_or(0);

        snapshots.push(SnapshotInfo {
            name: name.clone(),
            path: entry.path(),
            created_at,
            version,
            commit_count,
        });
    }

    // Sort newest first
    snapshots.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(snapshots)
}

/// Info about a single snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotInfo {
    pub name: String,
    pub path: PathBuf,
    pub created_at: Option<String>,
    pub version: String,
    pub commit_count: usize,
}

/// Restore a snapshot back to the kanban data directory.
///
/// This copies all parquet files + _commits.json from the snapshot back to
/// .nusy-kanban/, overwriting existing files. Requires --force to confirm.
pub fn restore_snapshot(
    snapshot_name: &str,
    config: &BackupConfig,
    root: &Path,
    force: bool,
) -> Result<PathBuf> {
    if !force {
        return Err(BackupError::RestoreRequiresForce);
    }

    let dest = config
        .destination
        .canonicalize()
        .unwrap_or_else(|_| config.destination.clone());

    let snapshot_dir = dest.join(snapshot_name);
    if !snapshot_dir.exists() {
        return Err(BackupError::SnapshotNotFound(snapshot_name.to_string()));
    }

    // Validate snapshot has required files
    let has_items = snapshot_dir.join("items.parquet").exists();
    if !has_items {
        return Err(BackupError::InvalidSnapshotName(
            "not a valid snapshot: missing items.parquet".to_string(),
        ));
    }

    let data_dir = root.join(".nusy-kanban");

    // Use explicit list to avoid cfg-related iterator issues
    let files = [
        "items.parquet",
        "runs.parquet",
        "comments.parquet",
        "item_comments.parquet",
        "relations.parquet",
        "experiment_runs.parquet",
        "_commits.json",
    ];
    if cfg!(feature = "pr") {
        let files_pr = ["proposals.parquet"];
        for filename in files.iter().chain(files_pr.iter()) {
            let src = snapshot_dir.join(filename);
            if src.exists() {
                let dst = data_dir.join(filename);
                fs::copy(&src, &dst)?;
            }
        }
    } else {
        for filename in files.iter() {
            let src = snapshot_dir.join(filename);
            if src.exists() {
                let dst = data_dir.join(filename);
                fs::copy(&src, &dst)?;
            }
        }
    }

    Ok(snapshot_dir)
}

/// Delete old snapshots beyond the retention limit.
pub fn prune_old_snapshots(dest: &Path, retention: usize) -> Result<()> {
    if retention == 0 {
        return Ok(()); // infinite retention
    }

    // List snapshot dirs (not the latest symlink)
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(dest) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("snapshot-")
                && entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false)
            {
                dirs.push(entry.path());
            }
        }
    }

    // Sort newest first
    dirs.sort_by(|a, b| b.cmp(a));

    // Remove oldest beyond retention limit
    for old in dirs.into_iter().skip(retention) {
        println!(
            "Pruning old snapshot: {}",
            old.file_name().unwrap_or_default().to_string_lossy()
        );
        fs::remove_dir_all(&old)?;
    }

    Ok(())
}

/// Check if a backup is due based on schedule and last backup timestamp.
pub fn is_backup_due(config: &BackupConfig) -> Result<bool> {
    let latest = config.destination.join("latest");
    if !latest.exists() {
        return Ok(true); // never backed up
    }

    // Probe destination reachability before attempting I/O on the symlink target.
    // If the backup volume is unmounted/unreachable, File::open on the symlink
    // itself will fail (ENOENT / stale NFS handle), and we treat backup as due.
    if File::open(&latest).is_err() {
        return Ok(true); // destination unreachable, assume stale/due
    }

    // Read metadata from latest snapshot
    let meta_path = latest.join("_metadata.json");
    if !meta_path.exists() {
        return Ok(true);
    }

    let file = File::open(&meta_path)?;
    let reader = BufReader::new(file);
    let metadata: SnapshotMetadata = serde_json::from_reader(reader)?;

    let last_backup = chrono::DateTime::parse_from_rfc3339(&metadata.created_at)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok();

    let now = chrono::Utc::now();

    let schedule = &config.schedule;
    if schedule == "hourly" {
        if let Some(last) = last_backup {
            return Ok((now - last).num_minutes() >= 60);
        }
    } else if schedule == "daily" {
        if let Some(last) = last_backup {
            return Ok((now - last).num_hours() >= 24);
        }
    } else if schedule == "weekly" {
        if let Some(last) = last_backup {
            return Ok((now - last).num_days() >= 7);
        }
    } else {
        // Try to parse as cron expression (5-field). For simplicity,
        // treat unrecognized as daily.
        eprintln!(
            "Warning: unrecognized schedule '{}', treating as daily",
            schedule
        );
        if let Some(last) = last_backup {
            return Ok((now - last).num_hours() >= 24);
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_name_timestamp() {
        let name = snapshot_name(BackupFormat::Timestamp);
        assert!(name.starts_with("snapshot-20"));
        assert!(name.len() >= "snapshot-2026-04-07_120000".len());
    }

    #[test]
    fn test_snapshot_name_date() {
        let name = snapshot_name(BackupFormat::Date);
        assert!(name.starts_with("snapshot-20"));
        assert!(!name.contains("_120000")); // no time component
    }

    #[test]
    fn test_parse_snapshot_date() {
        assert!(parse_snapshot_date("snapshot-2026-04-07").is_some());
        assert!(parse_snapshot_date("snapshot-2026-04-07_055839").is_some());
        assert!(parse_snapshot_date("latest").is_none());
        assert!(parse_snapshot_date("notasnapshot").is_none());
    }

    #[test]
    fn test_backup_format_display() {
        assert_eq!(format!("{}", BackupFormat::Timestamp), "timestamp");
        assert_eq!(format!("{}", BackupFormat::Date), "date");
    }
}
