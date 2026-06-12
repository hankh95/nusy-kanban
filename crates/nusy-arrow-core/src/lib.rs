//! nusy-arrow-core — Arrow-native graph store for NuSy.
//!
//! Foundation crate providing:
//! - Arrow schemas for triples, embeddings, and metadata
//! - 5-namespace partitioning (world/work/code/research/self)
//! - Y-layer partitioning (Y0-Y6)
//! - Zero-copy CRUD operations on Arrow RecordBatches
//! - Cognitive parameter store for V15 self-evolution

pub mod artifacts;
pub mod certainty;
pub mod cognitive_params;
pub mod epistemic;
pub mod graph_factory;
pub mod kg_store;
pub mod literals;
pub mod namespace;
pub mod parquet_atomic;
pub mod schema;
pub mod store;
pub mod triple_store;
pub mod y_layer;

pub use parquet_atomic::write_parquet_atomic;

pub use artifacts::{
    ArtifactError, ArtifactStatus, ArtifactStore, DepType, KnowledgeArtifact, Version,
};
pub use cognitive_params::{
    AutonomyTier, CognitiveParameter, CognitiveParameterStore, ParamStoreError,
    cognitive_params_schema, default_cognitive_params, default_signal_weights,
    init_cognitive_params, load_params_from_parquet, param_col, save_params_to_parquet,
};
pub use graph_factory::{
    CreatedStore, GraphBackend, GraphStoreConfig, HardwareCapabilities, available_backends,
    create_default_store, create_graph_store, detect_hardware, recommended_backend,
};
pub use literals::{TypedValue, compare_objects, parse as parse_typed_literal};
pub use namespace::Namespace;
pub use schema::chunk_col;
pub use schema::col;
pub use schema::{
    ARTIFACT_DEPENDENCIES_SCHEMA_VERSION, CHUNKS_SCHEMA_VERSION,
    KNOWLEDGE_ARTIFACTS_SCHEMA_VERSION, TRIPLES_SCHEMA_VERSION, artifact_dependencies_schema,
    chunks_schema, embeddings_schema_with_dim, knowledge_artifacts_schema, normalize_to_current,
};
pub use schema::{artifact_col, artifact_dep_col};
pub use store::{ArrowGraphStore, CausalNode, QuerySpec, StoreError, Triple};
pub use y_layer::YLayer;
