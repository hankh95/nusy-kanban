# nusy-graph-query

[![Crates.io](https://img.shields.io/crates/v/nusy-graph-query.svg)](https://crates.io/crates/nusy-graph-query)
[![docs.rs](https://docs.rs/nusy-graph-query/badge.svg)](https://docs.rs/nusy-graph-query)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Graph-native semantic search for Arrow RecordBatches** — embeddings,
traversal, hybrid ranking, and caching.

`nusy-graph-query` provides the building blocks for semantic search and graph
traversal over [Apache Arrow](https://arrow.apache.org/) data. It's designed
for knowledge graphs stored as RecordBatches, where you need to combine
structural graph queries with semantic similarity search.

## Features

- **`EmbeddingProvider` trait** — pluggable embedding backends (hash-based
  deterministic, Ollama API, subprocess sentence-transformers)
- **Graph traversal** — generic BFS/DFS over Arrow edge tables, parameterized
  by column indices via `EdgeSchema`
- **Hybrid ranking** — combine structural graph scores with semantic similarity
  using configurable weights
- **Embedding cache** — content-hash invalidation with Parquet persistence,
  so embeddings survive restarts without recomputation
- **Zero-copy Arrow** — operates directly on `RecordBatch` columns, no
  intermediate materialization

## Quick Start

```rust
use nusy_graph_query::{
    HashEmbeddingProvider, EmbeddingProvider,
    cosine_similarity, semantic_search,
    EmbeddedItem,
};

// Create a deterministic embedding provider (for testing or small datasets)
let provider = HashEmbeddingProvider::new(384);

// Embed some text
let vectors = provider.embed_batch(&[
    "Alice knows Bob".to_string(),
    "Cat is an animal".to_string(),
]).unwrap();

// Compute similarity
let sim = cosine_similarity(&vectors[0], &vectors[1]);
println!("Similarity: {sim:.4}");
```

### Graph Traversal

```rust
use nusy_graph_query::traversal::*;
use arrow::array::{RecordBatch, StringArray};

// Define your edge schema (which columns hold source/target/predicate)
let schema = EdgeSchema {
    source_col: 0,
    target_col: 1,
    predicate_col: Some(2),
};

// BFS from a node, following "calls" edges up to depth 3
let reachable = bfs("main", &edges_batch, &schema, Direction::Forward, Some("calls"), 3);
for node in &reachable {
    println!("  {} (depth {})", node.id, node.depth);
}
```

### Hybrid Ranking

```rust
use nusy_graph_query::{hybrid_rank, HybridConfig, RankCandidate};

let config = HybridConfig {
    structural_weight: 0.6,
    semantic_weight: 0.4,
};

// Combine structural graph scores with semantic similarity
let results = hybrid_rank(&candidates, &embeddings, "search query", &provider, &config, 10)?;
```

## Installation

```toml
[dependencies]
nusy-graph-query = "0.14"
```

### Feature Flags

| Flag | Default | Description |
|------|---------|-------------|
| `subprocess` | off | Python sentence-transformers provider (requires Python + sentence-transformers) |
| `fastembed` | off | Local ONNX embedding via fastembed-rs (~2ms/chunk, no network) |

## Architecture

```
nusy-graph-query
  embedding.rs          — EmbeddingProvider trait, hash provider, cosine similarity
  traversal.rs          — BFS/DFS over Arrow edge RecordBatches
  hybrid_rank.rs        — Weighted structural + semantic scoring
  cache.rs              — Content-hash embedding cache (Parquet persistence)
  fastembed_provider.rs — Local ONNX provider (feature: fastembed)
  subprocess.rs         — Python subprocess provider (feature: subprocess)
```

The crate operates on standard Apache Arrow `RecordBatch` data. Graph edges
can be stored as either:

- **Edge tables** — separate RecordBatch with source/target/predicate columns
  (use `build_adjacency` + `bfs`)
- **List columns** — dependencies stored as `List<Utf8>` on each node
  (use `build_adjacency_from_list` + `bfs_with_adjacency`)

## Minimum Supported Rust Version

Rust 2024 edition (1.85+). Uses `let-else` and `let chains`.

## Part of the NuSy Ecosystem

This crate is part of [nusy-product-team](https://github.com/hankh95/nusy-product-team),
a neurosymbolic AI platform. Related crates:

- **nusy-arrow-core** — Arrow schemas, Triple type, Namespace/YLayer enums
- **nusy-arrow-git** — Graph-native git operations on Arrow tables
- **nusy-dual-store** — Fast/slow dual-store with consolidation

## License

MIT
