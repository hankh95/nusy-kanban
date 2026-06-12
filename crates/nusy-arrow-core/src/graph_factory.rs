//! Graph store factory — select and create the right store backend.
//!
//! Replaces Python `brain/knowledge/graph_factory.py`. The Python factory
//! selected between GPU (cuDF) and CPU (Polars) backends. In V14 Rust,
//! all backends use Arrow. The factory selects the right *level* of store
//! based on use case and persistence requirements.
//!
//! # Backends
//!
//! | Backend | Store Type | Persistence | Use Case |
//! |---------|-----------|-------------|----------|
//! | `InMemory` | `ArrowGraphStore` | None | Tests, short-lived queries |
//! | `KnowledgeGraph` | `KgStore` | None | Prefix-aware KG operations |
//! | `Simple` | `SimpleTripleStore` | None | Default namespace + Y-layer |
//!
//! For persistent (hot/cold + Parquet) backends, use `nusy-dual-store::DualStore`
//! directly — it has its own `DualStoreConfig` builder.
//!
//! # Hardware Detection
//!
//! `detect_backends()` reports available compute capabilities. Currently all
//! backends are CPU (Arrow). GPU acceleration (CUDA via Candle) is tracked
//! separately in V6 and does not affect graph store selection.

use crate::kg_store::KgStore;
use crate::namespace::Namespace;
use crate::store::ArrowGraphStore;
use crate::triple_store::SimpleTripleStore;
use crate::y_layer::YLayer;

// ── Backend selection ───────────────────────────────────────────────────

/// Available graph store backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphBackend {
    /// Raw Arrow graph store — no defaults, no extras.
    /// Best for: tests, manual namespace management, maximum control.
    InMemory,

    /// Simple triple store with default namespace + Y-layer.
    /// Best for: single-namespace work, quick prototyping.
    Simple,

    /// Full knowledge graph with prefix management and keyword search.
    /// Best for: being knowledge, ontology work, gap tracking.
    KnowledgeGraph,
}

/// Hardware capabilities detected on this machine.
#[derive(Debug, Clone)]
pub struct HardwareCapabilities {
    /// Whether CUDA GPU is available (DGX).
    pub cuda_available: bool,
    /// Whether MPS (Metal) is available (M4/M5 Macs).
    pub mps_available: bool,
    /// Number of CPU cores.
    pub cpu_cores: usize,
    /// Approximate available memory in bytes.
    pub memory_bytes: u64,
}

/// Detect available hardware capabilities.
///
/// Currently reports CPU-only for graph stores. GPU availability is
/// informational — graph stores don't use GPU (training does, via V6).
pub fn detect_hardware() -> HardwareCapabilities {
    let cpu_cores = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1);

    // CUDA detection: check for nvidia-smi
    let cuda_available = std::process::Command::new("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    // MPS detection: macOS with Apple Silicon
    let mps_available = cfg!(target_os = "macos") && cfg!(target_arch = "aarch64");

    HardwareCapabilities {
        cuda_available,
        mps_available,
        cpu_cores,
        memory_bytes: 0, // Not easily portable; callers can override
    }
}

/// List available graph backends on this machine.
///
/// All three backends are always available (they're CPU Arrow).
/// This function exists for parity with Python's `get_available_backends()`.
pub fn available_backends() -> Vec<GraphBackend> {
    vec![
        GraphBackend::InMemory,
        GraphBackend::Simple,
        GraphBackend::KnowledgeGraph,
    ]
}

/// Select the recommended backend based on use case.
///
/// This is the main factory entry point. For most being work,
/// `KnowledgeGraph` is the right choice.
pub fn recommended_backend() -> GraphBackend {
    GraphBackend::KnowledgeGraph
}

// ── Factory configuration ───────────────────────────────────────────────

/// Configuration for graph store creation.
#[derive(Debug, Clone)]
pub struct GraphStoreConfig {
    /// Which backend to use.
    pub backend: GraphBackend,
    /// Default namespace (for Simple and KnowledgeGraph backends).
    pub default_namespace: Namespace,
    /// Default Y-layer (for Simple and KnowledgeGraph backends).
    pub default_y_layer: YLayer,
}

impl Default for GraphStoreConfig {
    fn default() -> Self {
        Self {
            backend: GraphBackend::KnowledgeGraph,
            default_namespace: Namespace::World,
            default_y_layer: YLayer::Semantic,
        }
    }
}

impl GraphStoreConfig {
    /// Create config for a specific backend.
    pub fn new(backend: GraphBackend) -> Self {
        Self {
            backend,
            ..Default::default()
        }
    }

    /// Set the default namespace.
    pub fn with_namespace(mut self, ns: Namespace) -> Self {
        self.default_namespace = ns;
        self
    }

    /// Set the default Y-layer.
    pub fn with_y_layer(mut self, layer: YLayer) -> Self {
        self.default_y_layer = layer;
        self
    }
}

// ── Factory functions ───────────────────────────────────────────────────

/// Created graph store — enum dispatch over backend types.
///
/// This avoids trait objects while still providing a unified return type.
/// Callers match on the variant to get the concrete store.
pub enum CreatedStore {
    InMemory(ArrowGraphStore),
    Simple(SimpleTripleStore),
    KnowledgeGraph(KgStore),
}

impl CreatedStore {
    /// Get the number of triples in the store.
    pub fn len(&self) -> usize {
        match self {
            Self::InMemory(s) => s.len(),
            Self::Simple(s) => s.len(),
            Self::KnowledgeGraph(s) => s.len(),
        }
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Unwrap as ArrowGraphStore (panics if wrong variant).
    pub fn into_arrow(self) -> ArrowGraphStore {
        match self {
            Self::InMemory(s) => s,
            _ => panic!("expected InMemory variant"),
        }
    }

    /// Unwrap as SimpleTripleStore (panics if wrong variant).
    pub fn into_simple(self) -> SimpleTripleStore {
        match self {
            Self::Simple(s) => s,
            _ => panic!("expected Simple variant"),
        }
    }

    /// Unwrap as KgStore (panics if wrong variant).
    pub fn into_kg(self) -> KgStore {
        match self {
            Self::KnowledgeGraph(s) => s,
            _ => panic!("expected KnowledgeGraph variant"),
        }
    }

    /// Try to unwrap as ArrowGraphStore.
    pub fn try_into_arrow(self) -> Option<ArrowGraphStore> {
        match self {
            Self::InMemory(s) => Some(s),
            _ => None,
        }
    }

    /// Try to unwrap as SimpleTripleStore.
    pub fn try_into_simple(self) -> Option<SimpleTripleStore> {
        match self {
            Self::Simple(s) => Some(s),
            _ => None,
        }
    }

    /// Try to unwrap as KgStore.
    pub fn try_into_kg(self) -> Option<KgStore> {
        match self {
            Self::KnowledgeGraph(s) => Some(s),
            _ => None,
        }
    }
}

/// Create a graph store based on configuration.
///
/// This is the primary factory function. Equivalent to Python's
/// `create_graph_store(backend="auto")`.
pub fn create_graph_store(config: &GraphStoreConfig) -> CreatedStore {
    match config.backend {
        GraphBackend::InMemory => CreatedStore::InMemory(ArrowGraphStore::new()),
        GraphBackend::Simple => CreatedStore::Simple(SimpleTripleStore::with_defaults(
            config.default_namespace,
            config.default_y_layer,
        )),
        GraphBackend::KnowledgeGraph => CreatedStore::KnowledgeGraph(KgStore::with_defaults(
            config.default_namespace,
            config.default_y_layer,
        )),
    }
}

/// Create a graph store with default configuration (KnowledgeGraph backend).
pub fn create_default_store() -> KgStore {
    KgStore::new()
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_hardware() {
        let hw = detect_hardware();
        assert!(hw.cpu_cores >= 1);
        // MPS should be true on Apple Silicon Macs
        if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
            assert!(hw.mps_available);
        }
    }

    #[test]
    fn test_available_backends() {
        let backends = available_backends();
        assert_eq!(backends.len(), 3);
        assert!(backends.contains(&GraphBackend::InMemory));
        assert!(backends.contains(&GraphBackend::Simple));
        assert!(backends.contains(&GraphBackend::KnowledgeGraph));
    }

    #[test]
    fn test_recommended_backend() {
        assert_eq!(recommended_backend(), GraphBackend::KnowledgeGraph);
    }

    #[test]
    fn test_create_in_memory() {
        let config = GraphStoreConfig::new(GraphBackend::InMemory);
        let store = create_graph_store(&config);
        assert!(store.is_empty());
        let arrow = store.into_arrow();
        assert_eq!(arrow.len(), 0);
    }

    #[test]
    fn test_create_simple() {
        let config = GraphStoreConfig::new(GraphBackend::Simple)
            .with_namespace(Namespace::Research)
            .with_y_layer(YLayer::Reasoning);
        let store = create_graph_store(&config);
        assert!(store.is_empty());
        let simple = store.into_simple();
        assert_eq!(simple.len(), 0);
    }

    #[test]
    fn test_create_knowledge_graph() {
        let config = GraphStoreConfig::new(GraphBackend::KnowledgeGraph);
        let store = create_graph_store(&config);
        assert!(store.is_empty());
        let kg = store.into_kg();
        // KgStore comes with default prefixes
        assert!(!kg.prefixes().is_empty());
    }

    #[test]
    fn test_create_default_store() {
        let store = create_default_store();
        assert!(store.is_empty());
        assert!(!store.prefixes().is_empty());
    }

    #[test]
    fn test_default_config() {
        let config = GraphStoreConfig::default();
        assert_eq!(config.backend, GraphBackend::KnowledgeGraph);
        assert_eq!(config.default_namespace, Namespace::World);
        assert_eq!(config.default_y_layer, YLayer::Semantic);
    }

    #[test]
    fn test_try_into_wrong_variant() {
        let store = create_graph_store(&GraphStoreConfig::new(GraphBackend::InMemory));
        assert!(store.try_into_kg().is_none());

        let store = create_graph_store(&GraphStoreConfig::new(GraphBackend::KnowledgeGraph));
        assert!(store.try_into_arrow().is_none());
    }

    #[test]
    fn test_created_store_len() {
        let store = create_graph_store(&GraphStoreConfig::new(GraphBackend::InMemory));
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn test_graceful_fallback_no_gpu() {
        // On Mini/M5 (no CUDA), factory should still work — all backends are CPU
        let _hw = detect_hardware();
        // Regardless of CUDA availability, all backends work
        for backend in available_backends() {
            let config = GraphStoreConfig::new(backend);
            let store = create_graph_store(&config);
            assert!(store.is_empty());
        }
    }
}
