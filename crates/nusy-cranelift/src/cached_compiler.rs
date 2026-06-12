//! CachedWasmCompiler — content-addressed compilation cache for WASM (V13-3, EX-3178).
//!
//! Wraps `WasmCompiler` with transparent caching via `BuildCache` from nusy-codegraph.
//! Before compiling a CodeNode, checks if its `body_hash` is already cached.
//! If hit, returns cached WASM bytes without recompilation.
//!
//! ## Performance
//!
//! A 2,000-function workspace should compile incrementally in < 30 seconds:
//! only changed functions recompile, everything else is a cache hit.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use nusy_codegraph::build_cache::{BuildCache, CacheEntry};
use nusy_codegraph::schema::CodeNode;
use sha2::{Digest, Sha256};

use crate::error::CraneliftError;
use crate::wasm_compiler::{WasmCompiler, WasmModule, WasmValue};

/// Compilation cache statistics using atomics (lock-free reads).
#[derive(Debug)]
pub struct CachedStats {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
}

impl CachedStats {
    fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Hit rate as a fraction [0.0, 1.0].
    pub fn hit_rate(&self) -> f64 {
        let h = self.hits.load(Ordering::Relaxed);
        let m = self.misses.load(Ordering::Relaxed);
        let total = h + m;
        if total == 0 {
            return 0.0;
        }
        h as f64 / total as f64
    }

    /// Snapshot of current stats.
    pub fn snapshot(&self) -> CachedStatsSnapshot {
        CachedStatsSnapshot {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }
}

/// Immutable snapshot of cache stats.
#[derive(Debug, Clone)]
pub struct CachedStatsSnapshot {
    pub hits: u64,
    pub misses: u64,
}

impl CachedStatsSnapshot {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        self.hits as f64 / total as f64
    }
}

/// WASM compiler with transparent content-addressed caching.
///
/// Before compiling a CodeNode:
/// 1. Compute SHA-256 of `node.body`
/// 2. Check cache — if hit, return cached WASM bytes
/// 3. If miss — compile via `WasmCompiler`, store in cache, return
pub struct CachedWasmCompiler {
    inner: WasmCompiler,
    cache: BuildCache,
    /// Cached WASM artifacts: body_hash → (function_name, wasm_bytes).
    /// Stored separately from BuildCache because BuildCache holds Arrow-backed
    /// metadata (compile time, size) while this holds the actual artifact bytes.
    wasm_store: std::collections::HashMap<String, (String, Vec<u8>)>,
    /// Reverse index: node_id → body_hash for invalidation by node ID.
    node_index: std::collections::HashMap<String, String>,
    stats: CachedStats,
}

impl CachedWasmCompiler {
    /// Create a new cached compiler.
    pub fn new(compiler: WasmCompiler) -> Self {
        Self {
            inner: compiler,
            cache: BuildCache::new(),
            wasm_store: std::collections::HashMap::new(),
            node_index: std::collections::HashMap::new(),
            stats: CachedStats::new(),
        }
    }

    /// Create with a pre-populated cache (e.g., loaded from Parquet).
    pub fn with_cache(compiler: WasmCompiler, cache: BuildCache) -> Self {
        Self {
            inner: compiler,
            cache,
            wasm_store: std::collections::HashMap::new(),
            node_index: std::collections::HashMap::new(),
            stats: CachedStats::new(),
        }
    }

    /// Compile a CodeNode, using cache if available.
    ///
    /// Returns the compiled WASM module. If the node's body_hash is already
    /// cached, the cached WASM bytes are returned without recompilation.
    pub fn compile(&mut self, node: &CodeNode) -> Result<WasmModule, CraneliftError> {
        let body = node.body.as_deref().ok_or(CraneliftError::MissingBody)?;
        let body_hash = compute_body_hash(body);

        // Check cache — both BuildCache entry and stored WASM artifact must exist
        if self.cache.get(&body_hash).is_some()
            && let Some((fn_name, wasm_bytes)) = self.wasm_store.get(&body_hash)
        {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);

            return Ok(WasmModule {
                node_id: node.id.clone(),
                function_name: fn_name.clone(),
                wasm_bytes: wasm_bytes.clone(),
            });
        }

        // Cache miss — compile
        self.stats.misses.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();
        let module = self.inner.compile(node)?;
        let compile_time_ns = start.elapsed().as_nanos() as i64;

        // Store in cache
        let entry = CacheEntry {
            body_hash: body_hash.clone(),
            artifact_kind: "wasm".to_string(),
            artifact_size: module.wasm_bytes.len() as u64,
            compile_time_ns,
            created_at: chrono::Utc::now().timestamp_millis(),
        };
        self.cache.put(entry);
        self.wasm_store.insert(
            body_hash.clone(),
            (module.function_name.clone(), module.wasm_bytes.clone()),
        );
        self.node_index.insert(node.id.clone(), body_hash);

        Ok(module)
    }

    /// Compile and execute in one step, using cache.
    pub fn compile_and_run(
        &mut self,
        node: &CodeNode,
        args: &[WasmValue],
    ) -> Result<WasmValue, CraneliftError> {
        let module = self.compile(node)?;
        self.inner.execute(&module, args)
    }

    /// Invalidate cache entries for a specific body hash.
    pub fn invalidate(&mut self, body_hash: &str) {
        self.wasm_store.remove(body_hash);
        self.cache.remove(body_hash);
    }

    /// Invalidate cache for a node by its ID.
    ///
    /// When a CodeNode's body is edited (agent edit, self-modification),
    /// call this to remove stale cached artifacts. The next `compile()`
    /// will recompile with the new body.
    pub fn invalidate_by_node_id(&mut self, node_id: &str) {
        if let Some(body_hash) = self.node_index.remove(node_id) {
            self.wasm_store.remove(&body_hash);
            self.cache.remove(&body_hash);
        }
    }

    /// Get the hit rate.
    pub fn hit_rate(&self) -> f64 {
        self.stats.hit_rate()
    }

    /// Get a stats snapshot.
    pub fn stats_snapshot(&self) -> CachedStatsSnapshot {
        self.stats.snapshot()
    }

    /// Get the underlying BuildCache (for persistence).
    pub fn cache(&self) -> &BuildCache {
        &self.cache
    }

    /// Number of cached WASM modules.
    pub fn cached_count(&self) -> usize {
        self.wasm_store.len()
    }
}

// ── Build Workspace (Phase 3) ───────────────────────────────────────────────

/// Result of a workspace build.
#[derive(Debug)]
pub struct BuildReport {
    /// Total functions compiled (hits + misses).
    pub total: usize,
    /// Functions where cache was used.
    pub cache_hits: u64,
    /// Functions that required recompilation.
    pub cache_misses: u64,
    /// Total compilation time.
    pub elapsed: std::time::Duration,
    /// Per-function errors (node_id → error message).
    pub errors: Vec<(String, String)>,
}

/// Build all CodeNodes in a workspace using the cached compiler.
///
/// Compiles each function node in the provided list (should be in topological
/// order from `sort_functions_in_crate`). Returns a report with cache stats.
pub fn build_workspace(nodes: &[CodeNode], compiler: &mut CachedWasmCompiler) -> BuildReport {
    let start = Instant::now();
    let before = compiler.stats_snapshot();
    let mut errors = Vec::new();

    for node in nodes {
        if node.body.is_none() {
            continue;
        }
        if let Err(e) = compiler.compile(node) {
            errors.push((node.id.clone(), e.to_string()));
        }
    }

    let after = compiler.stats_snapshot();
    BuildReport {
        total: nodes.len(),
        cache_hits: after.hits - before.hits,
        cache_misses: after.misses - before.misses,
        elapsed: start.elapsed(),
        errors,
    }
}

/// Compute SHA-256 hash of a function body.
fn compute_body_hash(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusy_codegraph::schema::CodeNodeKind;

    fn make_node(id: &str, body: &str) -> CodeNode {
        CodeNode {
            id: id.into(),
            kind: CodeNodeKind::RustFn,
            name: id.into(),
            body: Some(body.into()),
            ..CodeNode::default()
        }
    }

    fn compiler() -> CachedWasmCompiler {
        CachedWasmCompiler::new(WasmCompiler::new().expect("WasmCompiler"))
    }

    #[test]
    fn test_cache_hit_avoids_recompile() {
        let mut c = compiler();
        let node = make_node("add", "pub fn add(a: i64, b: i64) -> i64 { a + b }");

        // First compile — miss
        let start = Instant::now();
        let m1 = c.compile(&node).expect("first compile");
        let first_time = start.elapsed();

        // Second compile — hit
        let start = Instant::now();
        let m2 = c.compile(&node).expect("second compile");
        let second_time = start.elapsed();

        assert_eq!(m1.wasm_bytes, m2.wasm_bytes);
        assert_eq!(m1.function_name, m2.function_name);

        let stats = c.stats_snapshot();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert!((stats.hit_rate() - 0.5).abs() < 0.01);

        // Cache hit should be significantly faster
        assert!(
            second_time < first_time,
            "cache hit ({:?}) should be faster than miss ({:?})",
            second_time,
            first_time
        );
    }

    #[test]
    fn test_cache_miss_on_different_body() {
        let mut c = compiler();

        let n1 = make_node("add", "pub fn add(a: i64, b: i64) -> i64 { a + b }");
        let n2 = make_node("mul", "pub fn mul(a: i64, b: i64) -> i64 { a * b }");

        c.compile(&n1).expect("compile add");
        c.compile(&n2).expect("compile mul");

        let stats = c.stats_snapshot();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 2);
    }

    #[test]
    fn test_cache_invalidated_on_body_change() {
        let mut c = compiler();

        let node_v1 = make_node("calc", "pub fn calc(a: i64, b: i64) -> i64 { a + b }");
        c.compile(&node_v1).expect("compile v1");

        // "Edit" the function body
        let node_v2 = make_node("calc", "pub fn calc(a: i64, b: i64) -> i64 { a * b }");
        c.compile(&node_v2).expect("compile v2");

        // Both should be misses (different body_hash)
        let stats = c.stats_snapshot();
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.hits, 0);
    }

    #[test]
    fn test_incremental_build_only_recompiles_changed() {
        let mut c = compiler();

        // Compile 5 functions
        let nodes: Vec<CodeNode> = (0..5)
            .map(|i| {
                make_node(
                    &format!("fn{i}"),
                    &format!("pub fn fn{i}(a: i64) -> i64 {{ a + {i} }}"),
                )
            })
            .collect();

        for node in &nodes {
            c.compile(node).expect("compile");
        }
        assert_eq!(c.stats_snapshot().misses, 5);
        assert_eq!(c.stats_snapshot().hits, 0);

        // "Rebuild" all 5 — change only fn2
        let mut rebuild_nodes = nodes.clone();
        rebuild_nodes[2] = make_node("fn2", "pub fn fn2(a: i64) -> i64 { a * 100 }");

        for node in &rebuild_nodes {
            c.compile(node).expect("rebuild");
        }

        let stats = c.stats_snapshot();
        // 5 original misses + 1 miss (changed fn2) + 4 hits (unchanged)
        assert_eq!(stats.misses, 6, "should have 6 total misses");
        assert_eq!(stats.hits, 4, "should have 4 cache hits");
        assert!(stats.hit_rate() > 0.3); // 4/10 = 0.4
    }

    #[test]
    fn test_compile_and_run_uses_cache() {
        let mut c = compiler();
        let node = make_node("add", "pub fn add(a: i64, b: i64) -> i64 { a + b }");

        let r1 = c
            .compile_and_run(&node, &[WasmValue::I64(3), WasmValue::I64(4)])
            .expect("first run");
        let r2 = c
            .compile_and_run(&node, &[WasmValue::I64(10), WasmValue::I64(20)])
            .expect("second run");

        assert_eq!(r1, WasmValue::I64(7));
        assert_eq!(r2, WasmValue::I64(30));
        assert_eq!(c.stats_snapshot().hits, 1); // second compile was cached
    }

    #[test]
    fn test_cached_count() {
        let mut c = compiler();
        assert_eq!(c.cached_count(), 0);

        c.compile(&make_node("a", "pub fn a(x: i64) -> i64 { x }"))
            .expect("compile");
        assert_eq!(c.cached_count(), 1);

        c.compile(&make_node("b", "pub fn b(x: i64) -> i64 { x + 1 }"))
            .expect("compile");
        assert_eq!(c.cached_count(), 2);
    }

    #[test]
    fn test_body_hash_deterministic() {
        let h1 = compute_body_hash("pub fn add(a: i64, b: i64) -> i64 { a + b }");
        let h2 = compute_body_hash("pub fn add(a: i64, b: i64) -> i64 { a + b }");
        let h3 = compute_body_hash("pub fn add(a: i64, b: i64) -> i64 { a * b }");

        assert_eq!(h1, h2, "same body should produce same hash");
        assert_ne!(h1, h3, "different body should produce different hash");
        assert_eq!(h1.len(), 64, "SHA-256 hex should be 64 chars");
    }

    #[test]
    fn test_invalidate_by_node_id() {
        let mut c = compiler();
        let node = make_node("calc", "pub fn calc(a: i64, b: i64) -> i64 { a + b }");

        c.compile(&node).expect("compile");
        assert_eq!(c.cached_count(), 1);

        // Invalidate by node ID
        c.invalidate_by_node_id("calc");
        assert_eq!(c.cached_count(), 0);

        // Next compile should be a miss
        c.compile(&node).expect("recompile");
        let stats = c.stats_snapshot();
        assert_eq!(
            stats.misses, 2,
            "both compiles should be misses after invalidation"
        );
    }

    #[test]
    fn test_invalidate_nonexistent_node_is_noop() {
        let mut c = compiler();
        c.invalidate_by_node_id("nonexistent"); // should not panic
        assert_eq!(c.cached_count(), 0);
    }

    #[test]
    fn test_build_workspace_compiles_all() {
        let mut c = compiler();
        let nodes: Vec<CodeNode> = (0..3)
            .map(|i| {
                make_node(
                    &format!("fn{i}"),
                    &format!("pub fn fn{i}(a: i64) -> i64 {{ a + {i} }}"),
                )
            })
            .collect();

        let report = build_workspace(&nodes, &mut c);
        assert_eq!(report.total, 3);
        assert_eq!(report.cache_misses, 3);
        assert_eq!(report.cache_hits, 0);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn test_build_workspace_incremental() {
        let mut c = compiler();
        let nodes: Vec<CodeNode> = (0..5)
            .map(|i| {
                make_node(
                    &format!("fn{i}"),
                    &format!("pub fn fn{i}(a: i64) -> i64 {{ a + {i} }}"),
                )
            })
            .collect();

        // First build — all misses
        build_workspace(&nodes, &mut c);

        // Second build — all hits
        let report = build_workspace(&nodes, &mut c);
        assert_eq!(report.cache_hits, 5);
        assert_eq!(report.cache_misses, 0); // only new misses from this build... wait, stats are cumulative

        // Stats are cumulative: 5 misses from first + 5 hits from second
        let stats = c.stats_snapshot();
        assert_eq!(stats.misses, 5);
        assert_eq!(stats.hits, 5);
        assert!((stats.hit_rate() - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_build_workspace_reports_errors() {
        let mut c = compiler();
        let nodes = vec![
            make_node("good", "pub fn good(a: i64) -> i64 { a }"),
            make_node("bad", "this is not valid rust"),
        ];

        let report = build_workspace(&nodes, &mut c);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].0, "bad");
    }
}
