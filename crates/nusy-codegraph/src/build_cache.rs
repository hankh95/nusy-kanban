//! Content-addressed build cache — V12-Spike-2.
//!
//! Maps `body_hash` (SHA-256 of source text) to compiled artifacts.
//! If a CodeNode's body hasn't changed, its compiled output is reusable.
//!
//! # Architecture
//!
//! ```text
//! CodeNode.body_hash ──► BuildCache lookup
//!                            ├── hit  → return cached artifact (skip compilation)
//!                            └── miss → compile → store artifact → return
//! ```
//!
//! The cache is backed by an Arrow RecordBatch, persisted to Parquet.
//! This is a spike — validating that content addressing gives >90% cache
//! hits on real NuSy development before committing to the full V13 build.

use arrow::array::{
    ArrayRef, Int64Array, RecordBatch, StringArray, TimestampMillisecondArray, UInt64Array,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

// ─── Schema ─────────────────────────────────────────────────────────────────

/// Column indices for the build artifacts table.
pub mod artifact_col {
    pub const BODY_HASH: usize = 0;
    pub const ARTIFACT_KIND: usize = 1;
    pub const ARTIFACT_SIZE: usize = 2;
    pub const COMPILE_TIME_NS: usize = 3;
    pub const CREATED_AT: usize = 4;
}

/// Schema for the build artifacts cache table.
pub fn build_artifacts_schema() -> Schema {
    Schema::new(vec![
        Field::new("body_hash", DataType::Utf8, false),
        Field::new("artifact_kind", DataType::Utf8, false),
        Field::new("artifact_size", DataType::UInt64, false),
        Field::new("compile_time_ns", DataType::Int64, false),
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
    ])
}

// ─── Cache entry ────────────────────────────────────────────────────────────

/// A single cached build artifact.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub body_hash: String,
    pub artifact_kind: String,
    pub artifact_size: u64,
    pub compile_time_ns: i64,
    pub created_at: i64,
}

// ─── Stats ──────────────────────────────────────────────────────────────────

/// Cache performance statistics.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub total_artifact_bytes: u64,
    pub total_compile_time_saved_ns: i64,
}

impl CacheStats {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        self.hits as f64 / total as f64
    }
}

impl std::fmt::Display for CacheStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "hits={}, misses={}, hit_rate={:.1}%, saved={:.2}ms",
            self.hits,
            self.misses,
            self.hit_rate() * 100.0,
            self.total_compile_time_saved_ns as f64 / 1_000_000.0,
        )
    }
}

// ─── BuildCache ─────────────────────────────────────────────────────────────

/// Content-addressed build cache backed by Arrow.
///
/// Stores compiled artifacts keyed by the SHA-256 hash of the source body.
/// Same body → same hash → same compiled output → cache hit.
pub struct BuildCache {
    entries: HashMap<String, CacheEntry>,
    stats: CacheStats,
}

impl BuildCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            stats: CacheStats::default(),
        }
    }

    /// Look up a cached artifact by body hash.
    pub fn get(&mut self, body_hash: &str) -> Option<&CacheEntry> {
        if let Some(entry) = self.entries.get(body_hash) {
            self.stats.hits += 1;
            self.stats.total_compile_time_saved_ns += entry.compile_time_ns;
            Some(entry)
        } else {
            self.stats.misses += 1;
            None
        }
    }

    /// Store a compiled artifact in the cache.
    pub fn put(&mut self, entry: CacheEntry) {
        self.stats.total_artifact_bytes += entry.artifact_size;
        self.entries.insert(entry.body_hash.clone(), entry);
    }

    /// Number of cached artifacts.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Current cache statistics.
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Remove a cached artifact by body hash. Returns true if found.
    pub fn remove(&mut self, body_hash: &str) -> bool {
        self.entries.remove(body_hash).is_some()
    }

    /// Reset statistics counters (keeps cached entries).
    pub fn reset_stats(&mut self) {
        self.stats = CacheStats::default();
    }

    /// Serialize cache to an Arrow RecordBatch.
    pub fn to_record_batch(&self) -> Result<RecordBatch, arrow::error::ArrowError> {
        let schema = Arc::new(build_artifacts_schema());

        if self.entries.is_empty() {
            return Ok(RecordBatch::new_empty(schema));
        }

        let mut hashes = Vec::with_capacity(self.entries.len());
        let mut kinds = Vec::with_capacity(self.entries.len());
        let mut sizes = Vec::with_capacity(self.entries.len());
        let mut times = Vec::with_capacity(self.entries.len());
        let mut created = Vec::with_capacity(self.entries.len());

        for entry in self.entries.values() {
            hashes.push(entry.body_hash.as_str());
            kinds.push(entry.artifact_kind.as_str());
            sizes.push(entry.artifact_size);
            times.push(entry.compile_time_ns);
            created.push(entry.created_at);
        }

        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(hashes)) as ArrayRef,
                Arc::new(StringArray::from(kinds)) as ArrayRef,
                Arc::new(UInt64Array::from(sizes)) as ArrayRef,
                Arc::new(Int64Array::from(times)) as ArrayRef,
                Arc::new(TimestampMillisecondArray::from(created)) as ArrayRef,
            ],
        )
    }

    /// Deserialize cache from an Arrow RecordBatch.
    pub fn from_record_batch(batch: &RecordBatch) -> Result<Self, String> {
        let hashes = batch
            .column(artifact_col::BODY_HASH)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("body_hash column not StringArray")?;
        let kinds = batch
            .column(artifact_col::ARTIFACT_KIND)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("artifact_kind column not StringArray")?;
        let sizes = batch
            .column(artifact_col::ARTIFACT_SIZE)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or("artifact_size column not UInt64Array")?;
        let times = batch
            .column(artifact_col::COMPILE_TIME_NS)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or("compile_time_ns column not Int64Array")?;
        let created = batch
            .column(artifact_col::CREATED_AT)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .ok_or("created_at column not TimestampMillisecondArray")?;

        let mut entries = HashMap::new();
        for i in 0..batch.num_rows() {
            let entry = CacheEntry {
                body_hash: hashes.value(i).to_string(),
                artifact_kind: kinds.value(i).to_string(),
                artifact_size: sizes.value(i),
                compile_time_ns: times.value(i),
                created_at: created.value(i),
            };
            entries.insert(entry.body_hash.clone(), entry);
        }

        Ok(Self {
            entries,
            stats: CacheStats::default(),
        })
    }

    /// Save cache to a Parquet file.
    pub fn save_to_parquet(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let batch = self.to_record_batch()?;
        let file = std::fs::File::create(path)?;
        let mut writer = parquet::arrow::ArrowWriter::try_new(file, batch.schema(), None)?;
        writer.write(&batch)?;
        writer.close()?;
        Ok(())
    }

    /// Load cache from a Parquet file.
    pub fn load_from_parquet(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(path)?;
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReader::try_new(file, 8192)?;
        let mut all_entries = HashMap::new();
        for batch_result in reader {
            let batch = batch_result?;
            let partial = Self::from_record_batch(&batch)?;
            all_entries.extend(partial.entries);
        }
        Ok(Self {
            entries: all_entries,
            stats: CacheStats::default(),
        })
    }
}

impl Default for BuildCache {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Hash stability measurement ─────────────────────────────────────────────

/// Result of measuring hash stability across git commits.
#[derive(Debug, Clone)]
pub struct HashStabilityReport {
    /// Per-commit-pair measurements.
    pub measurements: Vec<CommitPairMeasurement>,
}

/// Measurement between two consecutive commits.
#[derive(Debug, Clone)]
pub struct CommitPairMeasurement {
    pub from_commit: String,
    pub to_commit: String,
    pub total_files: usize,
    pub unchanged_files: usize,
    pub changed_files: usize,
    pub cache_hit_rate: f64,
}

impl HashStabilityReport {
    /// Average cache hit rate across all commit pairs.
    pub fn avg_hit_rate(&self) -> f64 {
        if self.measurements.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.measurements.iter().map(|m| m.cache_hit_rate).sum();
        sum / self.measurements.len() as f64
    }

    /// Minimum cache hit rate observed.
    pub fn min_hit_rate(&self) -> f64 {
        self.measurements
            .iter()
            .map(|m| m.cache_hit_rate)
            .fold(f64::MAX, f64::min)
    }

    /// Maximum cache hit rate observed.
    pub fn max_hit_rate(&self) -> f64 {
        self.measurements
            .iter()
            .map(|m| m.cache_hit_rate)
            .fold(f64::MIN, f64::max)
    }

    /// Format as a human-readable report.
    pub fn summary(&self) -> String {
        format!(
            "Hash Stability Report ({} commit pairs)\n\
             ├── avg cache hit rate: {:.1}%\n\
             ├── min cache hit rate: {:.1}%\n\
             ├── max cache hit rate: {:.1}%\n\
             └── go signal (>90%):   {}",
            self.measurements.len(),
            self.avg_hit_rate() * 100.0,
            self.min_hit_rate() * 100.0,
            self.max_hit_rate() * 100.0,
            if self.avg_hit_rate() > 0.90 {
                "PASS ✓"
            } else {
                "FAIL ✗"
            }
        )
    }
}

/// Collect file hashes for a single git commit.
///
/// Returns a map of (file_path → blob_hash) for all .rs files under `prefix`.
/// Uses `git ls-tree` which gives us the blob hash — this IS content addressing.
pub fn collect_file_hashes(
    repo_root: &Path,
    commit: &str,
    prefix: &str,
) -> Result<HashMap<String, String>, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["ls-tree", "-r", commit, "--", prefix])
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "git ls-tree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let stdout = String::from_utf8(output.stdout)?;
    let mut hashes = HashMap::new();

    for line in stdout.lines() {
        // Format: <mode> <type> <hash>\t<path>
        let parts: Vec<&str> = line.splitn(4, [' ', '\t']).collect();
        if parts.len() == 4 && parts[3].ends_with(".rs") {
            hashes.insert(parts[3].to_string(), parts[2].to_string());
        }
    }

    Ok(hashes)
}

/// Get the last N commit hashes from git log.
pub fn recent_commits(
    repo_root: &Path,
    count: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["log", "--format=%H", "-n", &count.to_string()])
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let stdout = String::from_utf8(output.stdout)?;
    Ok(stdout.lines().map(String::from).collect())
}

/// Measure hash stability across recent git commits.
///
/// For each consecutive commit pair, compares the set of .rs file blob hashes.
/// Files with the same blob hash between commits = cache hits.
pub fn measure_hash_stability(
    repo_root: &Path,
    prefix: &str,
    commit_count: usize,
) -> Result<HashStabilityReport, Box<dyn std::error::Error>> {
    let commits = recent_commits(repo_root, commit_count)?;

    if commits.len() < 2 {
        return Ok(HashStabilityReport {
            measurements: vec![],
        });
    }

    let mut measurements = Vec::new();

    // Collect hashes for the most recent commit first
    let mut prev_hashes = collect_file_hashes(repo_root, &commits[0], prefix)?;

    // Walk backwards through history
    for i in 1..commits.len() {
        let curr_hashes = collect_file_hashes(repo_root, &commits[i], prefix)?;

        // Compare: files present in BOTH commits with the same hash = unchanged
        let all_files: std::collections::HashSet<&String> =
            prev_hashes.keys().chain(curr_hashes.keys()).collect();
        let total = all_files.len();

        let unchanged = all_files
            .iter()
            .filter(|f| {
                prev_hashes.get(**f) == curr_hashes.get(**f)
                    && prev_hashes.contains_key(**f)
                    && curr_hashes.contains_key(**f)
            })
            .count();

        let changed = total - unchanged;
        let hit_rate = if total > 0 {
            unchanged as f64 / total as f64
        } else {
            1.0
        };

        measurements.push(CommitPairMeasurement {
            from_commit: commits[i][..8].to_string(),
            to_commit: commits[i - 1][..8].to_string(),
            total_files: total,
            unchanged_files: unchanged,
            changed_files: changed,
            cache_hit_rate: hit_rate,
        });

        prev_hashes = curr_hashes;
    }

    Ok(HashStabilityReport { measurements })
}

// ─── Dependency-aware invalidation ───────────────────────────────────────────

/// Crate dependency graph for transitive invalidation analysis.
///
/// When a file in crate A changes, crate A needs recompilation.
/// If crate B depends on A, B also needs recompilation (its inputs changed).
/// This propagates transitively through the dependency graph.
pub struct CrateDeps {
    /// Crate name → list of crate names it depends on.
    pub deps: HashMap<String, Vec<String>>,
}

impl CrateDeps {
    /// Build the NuSy workspace dependency graph.
    ///
    /// Hardcoded for the spike — a production version would parse Cargo.toml
    /// or use `cargo metadata`.
    pub fn nusy_workspace() -> Self {
        let mut deps = HashMap::new();
        deps.insert("nusy-arrow-core".into(), vec![]);
        deps.insert(
            "nusy-arrow-git".into(),
            vec![
                "nusy-arrow-core".into(),
                "nusy-codegraph".into(),
                "nusy-kanban".into(),
            ],
        );
        deps.insert(
            "nusy-codegraph".into(),
            vec!["nusy-arrow-core".into(), "nusy-arrow-git".into()],
        );
        deps.insert("nusy-conductor".into(), vec![]);
        deps.insert("nusy-dual-store".into(), vec!["nusy-arrow-core".into()]);
        deps.insert("nusy-graph-adapter".into(), vec!["nusy-arrow-core".into()]);
        deps.insert(
            "nusy-graph-review".into(),
            vec!["nusy-arrow-core".into(), "nusy-arrow-git".into()],
        );
        deps.insert(
            "nusy-kanban".into(),
            vec![
                "nusy-arrow-core".into(),
                "nusy-arrow-git".into(),
                "nusy-codegraph".into(),
                "nusy-conductor".into(),
                "nusy-graph-review".into(),
            ],
        );
        deps.insert(
            "nusy-kanban-server".into(),
            vec!["nusy-kanban".into(), "nusy-graph-review".into()],
        );
        deps.insert("nusy-ontology".into(), vec!["nusy-arrow-core".into()]);
        deps.insert(
            "nusy-perceive".into(),
            vec![
                "nusy-arrow-core".into(),
                "nusy-dual-store".into(),
                "nusy-signal-fusion".into(),
            ],
        );
        deps.insert("nusy-reasoning-causal".into(), vec![]);
        deps.insert("nusy-signal-fusion".into(), vec!["nusy-arrow-core".into()]);
        Self { deps }
    }

    /// Compute reverse dependencies: crate → set of crates that depend on it.
    pub fn reverse_deps(&self) -> HashMap<String, Vec<String>> {
        let mut rdeps: HashMap<String, Vec<String>> = HashMap::new();
        for name in self.deps.keys() {
            rdeps.entry(name.clone()).or_default();
        }
        for (name, dep_list) in &self.deps {
            for dep in dep_list {
                rdeps.entry(dep.clone()).or_default().push(name.clone());
            }
        }
        rdeps
    }

    /// Given a set of directly dirty crates, compute the full set of crates
    /// needing recompilation (transitive dependents).
    pub fn transitive_dirty(
        &self,
        dirty: &std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String> {
        let rdeps = self.reverse_deps();
        let mut result = dirty.clone();
        let mut queue: Vec<String> = dirty.iter().cloned().collect();

        while let Some(crate_name) = queue.pop() {
            if let Some(dependents) = rdeps.get(&crate_name) {
                for dep in dependents {
                    if result.insert(dep.clone()) {
                        queue.push(dep.clone());
                    }
                }
            }
        }

        result
    }
}

/// Result of dependency-aware cache analysis.
#[derive(Debug, Clone)]
pub struct DependencyAwareReport {
    pub measurements: Vec<DepAwareMeasurement>,
}

/// Per-commit dependency-aware measurement.
#[derive(Debug, Clone)]
pub struct DepAwareMeasurement {
    pub from_commit: String,
    pub to_commit: String,
    pub total_crates: usize,
    pub directly_dirty: usize,
    pub transitively_dirty: usize,
    pub clean_crates: usize,
    /// File-level hit rate (upper bound).
    pub file_hit_rate: f64,
    /// Crate-level hit rate (what cargo sees).
    pub crate_hit_rate: f64,
    /// Dependency-aware hit rate (the real number).
    pub dep_aware_hit_rate: f64,
}

impl DependencyAwareReport {
    pub fn avg_dep_aware_hit_rate(&self) -> f64 {
        if self.measurements.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.measurements.iter().map(|m| m.dep_aware_hit_rate).sum();
        sum / self.measurements.len() as f64
    }

    pub fn summary(&self) -> String {
        let avg_file: f64 = self
            .measurements
            .iter()
            .map(|m| m.file_hit_rate)
            .sum::<f64>()
            / self.measurements.len().max(1) as f64;
        let avg_crate: f64 = self
            .measurements
            .iter()
            .map(|m| m.crate_hit_rate)
            .sum::<f64>()
            / self.measurements.len().max(1) as f64;
        let avg_dep = self.avg_dep_aware_hit_rate();

        format!(
            "Dependency-Aware Cache Analysis ({} commit pairs)\n\
             ├── file-level hit rate (upper bound):  {:.1}%\n\
             ├── crate-level hit rate:               {:.1}%\n\
             ├── dep-aware hit rate (real number):    {:.1}%\n\
             └── go signal (dep-aware > 90%):         {}",
            self.measurements.len(),
            avg_file * 100.0,
            avg_crate * 100.0,
            avg_dep * 100.0,
            if avg_dep > 0.90 {
                "PASS ✓"
            } else {
                "FAIL ✗"
            }
        )
    }
}

/// Extract crate name from a file path like "crates/nusy-arrow-core/src/lib.rs".
fn crate_from_path(path: &str) -> Option<&str> {
    let path = path.strip_prefix("crates/")?;
    path.split('/').next()
}

/// Measure dependency-aware cache hit rates across git history.
pub fn measure_dep_aware_stability(
    repo_root: &Path,
    commit_count: usize,
) -> Result<DependencyAwareReport, Box<dyn std::error::Error>> {
    let crate_deps = CrateDeps::nusy_workspace();
    let all_crates: std::collections::HashSet<String> = crate_deps.deps.keys().cloned().collect();
    let total_crates = all_crates.len();

    let commits = recent_commits(repo_root, commit_count)?;
    if commits.len() < 2 {
        return Ok(DependencyAwareReport {
            measurements: vec![],
        });
    }

    let mut measurements = Vec::new();
    let mut prev_hashes = collect_file_hashes(repo_root, &commits[0], "crates/")?;

    for i in 1..commits.len() {
        let curr_hashes = collect_file_hashes(repo_root, &commits[i], "crates/")?;

        // File-level: count unchanged files
        let all_files: std::collections::HashSet<&String> =
            prev_hashes.keys().chain(curr_hashes.keys()).collect();
        let file_total = all_files.len();
        let file_unchanged = all_files
            .iter()
            .filter(|f| {
                prev_hashes.get(**f) == curr_hashes.get(**f)
                    && prev_hashes.contains_key(**f)
                    && curr_hashes.contains_key(**f)
            })
            .count();
        let file_hit_rate = if file_total > 0 {
            file_unchanged as f64 / file_total as f64
        } else {
            1.0
        };

        // Crate-level: which crates have ANY changed file?
        let mut directly_dirty = std::collections::HashSet::new();
        for f in &all_files {
            let old = prev_hashes.get(*f);
            let new = curr_hashes.get(*f);
            if (old != new || old.is_none() || new.is_none())
                && let Some(crate_name) = crate_from_path(f)
            {
                directly_dirty.insert(crate_name.to_string());
            }
        }

        let crate_clean = total_crates - directly_dirty.len();
        let crate_hit_rate = crate_clean as f64 / total_crates as f64;

        // Dependency-aware: propagate dirtiness transitively
        let all_dirty = crate_deps.transitive_dirty(&directly_dirty);
        let dep_clean = total_crates - all_dirty.len();
        let dep_aware_hit_rate = dep_clean as f64 / total_crates as f64;

        measurements.push(DepAwareMeasurement {
            from_commit: commits[i][..8].to_string(),
            to_commit: commits[i - 1][..8].to_string(),
            total_crates,
            directly_dirty: directly_dirty.len(),
            transitively_dirty: all_dirty.len(),
            clean_crates: dep_clean,
            file_hit_rate,
            crate_hit_rate,
            dep_aware_hit_rate,
        });

        prev_hashes = curr_hashes;
    }

    Ok(DependencyAwareReport { measurements })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(hash: &str, kind: &str, size: u64) -> CacheEntry {
        CacheEntry {
            body_hash: hash.to_string(),
            artifact_kind: kind.to_string(),
            artifact_size: size,
            compile_time_ns: 50_000_000, // 50ms
            created_at: 1710700000000,
        }
    }

    #[test]
    fn test_cache_put_get() {
        let mut cache = BuildCache::new();
        assert!(cache.is_empty());

        cache.put(make_entry("abc123", "rlib", 4096));
        assert_eq!(cache.len(), 1);

        // Hit
        let entry = cache.get("abc123");
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().artifact_size, 4096);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 0);

        // Miss
        let miss = cache.get("nonexistent");
        assert!(miss.is_none());
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);
        assert!((cache.stats().hit_rate() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_cache_overwrite() {
        let mut cache = BuildCache::new();
        cache.put(make_entry("abc123", "rlib", 4096));
        cache.put(make_entry("abc123", "rlib", 8192)); // overwrite
        assert_eq!(cache.len(), 1);

        let entry = cache.get("abc123").unwrap();
        assert_eq!(entry.artifact_size, 8192);
    }

    #[test]
    fn test_arrow_round_trip() {
        let mut cache = BuildCache::new();
        cache.put(make_entry("hash_a", "rlib", 1024));
        cache.put(make_entry("hash_b", "rmeta", 2048));
        cache.put(make_entry("hash_c", "wasm", 512));

        let batch = cache.to_record_batch().unwrap();
        assert_eq!(batch.num_rows(), 3);
        assert_eq!(batch.num_columns(), 5);

        let restored = BuildCache::from_record_batch(&batch).unwrap();
        assert_eq!(restored.len(), 3);

        // Verify all entries survived
        let mut restored = restored;
        assert!(restored.get("hash_a").is_some());
        assert!(restored.get("hash_b").is_some());
        assert!(restored.get("hash_c").is_some());
        assert_eq!(restored.get("hash_a").unwrap().artifact_size, 1024);
    }

    #[test]
    fn test_parquet_round_trip() {
        let mut cache = BuildCache::new();
        cache.put(make_entry("hash_x", "rlib", 9999));
        cache.put(make_entry("hash_y", "wasm", 1111));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.parquet");

        cache.save_to_parquet(&path).unwrap();
        assert!(path.exists());

        let mut loaded = BuildCache::load_from_parquet(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get("hash_x").unwrap().artifact_size, 9999);
        assert_eq!(loaded.get("hash_y").unwrap().artifact_size, 1111);
    }

    #[test]
    fn test_empty_cache_operations() {
        let mut cache = BuildCache::new();
        assert!(cache.is_empty());
        assert!(cache.get("anything").is_none());
        assert_eq!(cache.stats().hit_rate(), 0.0);

        let batch = cache.to_record_batch().unwrap();
        assert_eq!(batch.num_rows(), 0);
    }

    #[test]
    fn test_stats_display() {
        let stats = CacheStats {
            hits: 90,
            misses: 10,
            total_artifact_bytes: 1024 * 1024,
            total_compile_time_saved_ns: 5_000_000_000,
        };
        let display = format!("{}", stats);
        assert!(display.contains("90.0%"));
        assert!(display.contains("5000.00ms"));
    }

    #[test]
    fn test_stability_report_summary() {
        let report = HashStabilityReport {
            measurements: vec![
                CommitPairMeasurement {
                    from_commit: "aaa".into(),
                    to_commit: "bbb".into(),
                    total_files: 100,
                    unchanged_files: 95,
                    changed_files: 5,
                    cache_hit_rate: 0.95,
                },
                CommitPairMeasurement {
                    from_commit: "bbb".into(),
                    to_commit: "ccc".into(),
                    total_files: 100,
                    unchanged_files: 98,
                    changed_files: 2,
                    cache_hit_rate: 0.98,
                },
            ],
        };
        assert!((report.avg_hit_rate() - 0.965).abs() < 0.001);
        assert!((report.min_hit_rate() - 0.95).abs() < f64::EPSILON);
        assert!((report.max_hit_rate() - 0.98).abs() < f64::EPSILON);
        let summary = report.summary();
        assert!(summary.contains("PASS"));
    }

    /// Integration test: measure hash stability on the actual NuSy codebase.
    ///
    /// This is the core spike measurement. It walks real git history and
    /// reports what cache hit rate we'd get with content-addressed builds.
    #[test]
    fn test_real_codebase_hash_stability() {
        // Find repo root (we're in crates/nusy-codegraph/)
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output();
        let repo_root = match output {
            Ok(o) if o.status.success() => {
                std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim())
            }
            _ => {
                eprintln!("Skipping: not in a git repo");
                return;
            }
        };

        // Measure across last 30 commits
        let report = measure_hash_stability(&repo_root, "crates/", 30).unwrap();

        if report.measurements.is_empty() {
            eprintln!("Skipping: not enough git history");
            return;
        }

        // Print detailed results
        println!("\n{}", report.summary());
        println!("\nPer-commit breakdown:");
        for m in &report.measurements {
            println!(
                "  {} → {}: {}/{} unchanged ({:.1}%)",
                m.from_commit,
                m.to_commit,
                m.unchanged_files,
                m.total_files,
                m.cache_hit_rate * 100.0,
            );
        }

        // The go signal: >90% average cache hit rate
        println!(
            "\nGo signal (avg > 90%): {:.1}% — {}",
            report.avg_hit_rate() * 100.0,
            if report.avg_hit_rate() > 0.90 {
                "PASS"
            } else {
                "FAIL"
            }
        );
    }

    #[test]
    fn test_transitive_dirty_propagation() {
        let deps = CrateDeps::nusy_workspace();

        // If nusy-arrow-core changes, everything that depends on it
        // (transitively) should be dirty
        let mut dirty = std::collections::HashSet::new();
        dirty.insert("nusy-arrow-core".to_string());

        let all_dirty = deps.transitive_dirty(&dirty);

        // arrow-core is the root dep — almost everything depends on it
        assert!(all_dirty.contains("nusy-arrow-core"));
        assert!(all_dirty.contains("nusy-arrow-git"));
        assert!(all_dirty.contains("nusy-codegraph"));
        assert!(all_dirty.contains("nusy-kanban"));
        assert!(all_dirty.contains("nusy-kanban-server")); // transitive via kanban

        // These have no internal deps — should NOT be dirty
        assert!(!all_dirty.contains("nusy-conductor")); // leaf, but kanban depends on it
        assert!(!all_dirty.contains("nusy-reasoning-causal")); // true leaf
    }

    #[test]
    fn test_leaf_crate_dirty_no_propagation() {
        let deps = CrateDeps::nusy_workspace();

        // If a leaf crate changes, only it should be dirty
        let mut dirty = std::collections::HashSet::new();
        dirty.insert("nusy-reasoning-causal".to_string());

        let all_dirty = deps.transitive_dirty(&dirty);
        assert_eq!(all_dirty.len(), 1);
        assert!(all_dirty.contains("nusy-reasoning-causal"));
    }

    /// Integration test: dependency-aware cache hit rates on real NuSy codebase.
    ///
    /// This is the REAL measurement — it accounts for transitive invalidation.
    /// File-level hits are the upper bound; this is the actual number.
    #[test]
    fn test_real_codebase_dep_aware_stability() {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output();
        let repo_root = match output {
            Ok(o) if o.status.success() => {
                std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim())
            }
            _ => {
                eprintln!("Skipping: not in a git repo");
                return;
            }
        };

        let report = measure_dep_aware_stability(&repo_root, 30).unwrap();

        if report.measurements.is_empty() {
            eprintln!("Skipping: not enough git history");
            return;
        }

        println!("\n{}", report.summary());
        println!("\nPer-commit breakdown:");
        for m in &report.measurements {
            println!(
                "  {} → {}: files={:.1}% crate={:.1}% dep-aware={:.1}% (dirty: {} direct, {} transitive of {} crates)",
                m.from_commit,
                m.to_commit,
                m.file_hit_rate * 100.0,
                m.crate_hit_rate * 100.0,
                m.dep_aware_hit_rate * 100.0,
                m.directly_dirty,
                m.transitively_dirty,
                m.total_crates,
            );
        }

        println!(
            "\nDep-aware go signal (avg > 90%): {:.1}% — {}",
            report.avg_dep_aware_hit_rate() * 100.0,
            if report.avg_dep_aware_hit_rate() > 0.90 {
                "PASS"
            } else {
                "NEEDS ANALYSIS"
            }
        );
    }
}
