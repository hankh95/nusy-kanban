# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.14.0] - 2026-03-18

### Added

- **`EmbeddingProvider` trait** — pluggable embedding backends with configurable dimension
- **`HashEmbeddingProvider`** — deterministic SHA-256 based embeddings for testing
- **`OllamaEmbeddingProvider`** — Ollama API embeddings (feature: `ollama`)
- **`SubprocessEmbeddingProvider`** — Python sentence-transformers (feature: `subprocess`)
- **`cosine_similarity`** — pairwise vector similarity
- **`semantic_search`** — rank items by embedding similarity to a query
- **`build_adjacency`** — build adjacency lists from Arrow edge RecordBatches
- **`build_adjacency_from_list`** — build adjacency from List<Utf8> columns (kanban depends_on)
- **`bfs` / `bfs_with_adjacency`** — breadth-first traversal with cycle detection and depth limits
- **`hybrid_rank`** — combine structural graph scores with semantic similarity
- **`EmbeddingCache`** — content-hash invalidation with Parquet persistence
- **`EdgeSchema`** — parameterize traversal by column indices (no hardcoded schema)

### Architecture

- Extracted from `nusy-kanban/embeddings.rs` and `nusy-codegraph/embeddings.rs`
  (EX-3145) — eliminated ~300 lines of duplication
- Arrow-native: operates directly on `RecordBatch` columns
- Zero external runtime dependencies (default features)
