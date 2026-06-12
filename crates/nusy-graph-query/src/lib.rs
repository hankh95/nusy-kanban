//! # nusy-graph-query
//!
//! Graph-native semantic search for Arrow RecordBatches — embeddings,
//! traversal, hybrid ranking, and caching.
//!
//! This crate provides the building blocks for combining structural graph
//! queries with semantic similarity search over Apache Arrow data.
//!
//! ## Quick Example
//!
//! ```rust
//! use nusy_graph_query::{HashEmbeddingProvider, EmbeddingProvider, cosine_similarity};
//!
//! let provider = HashEmbeddingProvider::new(384);
//! let vecs = provider.embed_batch(&[
//!     "Alice knows Bob".to_string(),
//!     "Cat is an animal".to_string(),
//! ]).unwrap();
//!
//! let sim = cosine_similarity(&vecs[0], &vecs[1]);
//! assert!(sim >= -1.0 && sim <= 1.0);
//! ```
//!
//! ## Modules
//!
//! - [`embedding`] — `EmbeddingProvider` trait, hash provider, cosine similarity
//! - [`traversal`] — Generic BFS/DFS over Arrow edge RecordBatches
//! - [`hybrid_rank`] — Combine structural + semantic scores
//! - [`cache`] — Content-hash embedding cache with Parquet persistence
//! - [`subprocess`] — Python sentence-transformers provider (feature: `subprocess`)
//! - [`fastembed_provider`] — Local ONNX embedding provider (feature: `fastembed`)
//!
//! ## Feature Flags
//!
//! | Flag | Description |
//! |------|-------------|
//! | `subprocess` | Enable Python sentence-transformers provider |
//! | `fastembed` | Enable local ONNX embedding via fastembed-rs (~2ms/chunk) |

pub mod cache;
pub mod embedding;
#[cfg(feature = "fastembed")]
pub mod fastembed_provider;
pub mod hybrid_rank;
#[cfg(feature = "subprocess")]
pub mod subprocess;
pub mod traversal;

// Re-export key types at crate root for convenience.
pub use cache::EmbeddingCache;
pub use embedding::{
    EmbeddedItem, EmbeddingError, EmbeddingProvider, HashEmbeddingProvider, SearchResult,
    cosine_similarity, hash_to_vector, semantic_search,
};
#[cfg(feature = "fastembed")]
pub use fastembed_provider::FastembedProvider;
pub use hybrid_rank::{HybridConfig, RankCandidate, RankedResult, hybrid_rank};
#[cfg(feature = "subprocess")]
pub use subprocess::SubprocessEmbeddingProvider;
pub use traversal::{
    Direction, EdgeSchema, TraversalNode, bfs, bfs_with_adjacency, build_adjacency,
    build_adjacency_from_list,
};
