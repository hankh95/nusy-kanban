# Changelog

All notable changes to this project will be documented in this file.

## [0.14.3] — 2026-04-05

### Fixed
- README: Add Arrow schema diagram showing items/runs/relations/comments tables
- README: Add persistence flow diagram (WAL + atomic rename)
- README: Add troubleshooting section (NATS failures, build errors, schema migration)
- CLI-REFERENCE.md: Fix typos and clarify flag descriptions

### Changed
- serde_json dependency made unconditional (fixes `--no-default-features` build)

## [0.14.2] — 2026-04-01

### Added
- Graph-native PR workflows via `nk pr` subcommand
- NATS JetStream event broadcasting
- SHACL shape validation for all 13 item types
- HDD research board with 6 research types
- Training queue via NATS KV
- MCP tools (query, show, create, move, update, relations, stats, schemas)

### Features
- Arrow/Parquet persistence with WAL + atomic rename
- Dual boards (development + research)
- Natural-language query via embeddings (fastembed/ollama)
- Critical path and roadmap analysis

## [0.14.1] — 2026-03-15

Initial FOSS release.
