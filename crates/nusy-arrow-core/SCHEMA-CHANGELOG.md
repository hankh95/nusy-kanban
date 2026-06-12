# Schema Changelog — nusy-arrow-core

Documents schema evolution for the Arrow-native graph substrate.
Schema versions are stored in Parquet file metadata (`nusy_schema_version`).
The `normalize_to_current()` function handles read-path migration.

## v1.1.0 (2026-03-15) — ChunkTable + Fine-Grained Provenance

**EXP-1278 / CHORE-158**

### Triples Schema (15 → 16 columns)

- **Added:** `source_chunk_id: Utf8` (nullable) at index 9
  - FK to ChunkTable for paragraph-level provenance
  - Existing triples have `source_chunk_id = null` (falls back to `source_document`)
  - All column indices after index 8 shifted by 1

| Index | v1.0.0 | v1.1.0 |
|-------|--------|--------|
| 0-8 | (unchanged) | (unchanged) |
| 9 | `extracted_by` | **`source_chunk_id`** (NEW) |
| 10 | `created_at` | `extracted_by` |
| 11 | `caused_by` | `created_at` |
| 12 | `derived_from` | `caused_by` |
| 13 | `consolidated_at` | `derived_from` |
| 14 | `deleted` | `consolidated_at` |
| 15 | — | `deleted` |

### New: Chunks Schema (17 columns)

| Index | Column | Type | Nullable | Description |
|-------|--------|------|----------|-------------|
| 0 | `chunk_id` | Utf8 | no | PK: `chunk_{doc_hash}_{index}` |
| 1 | `document_path` | Utf8 | no | Source document path/URI |
| 2 | `content` | LargeUtf8 | yes | Full chunk text |
| 3 | `token_count` | UInt32 | no | Estimated tokens |
| 4 | `chunk_index` | UInt32 | no | 0-based position in document |
| 5 | `total_chunks` | UInt32 | no | Total chunks in document |
| 6 | `char_offset_start` | UInt64 | yes | Character offset from doc start |
| 7 | `char_offset_end` | UInt64 | yes | Character offset end |
| 8 | `page_number` | UInt32 | yes | Page (null if not paginated) |
| 9 | `section_heading` | Utf8 | yes | Nearest section heading |
| 10 | `section_level` | UInt8 | yes | Heading level (1-6) |
| 11 | `paragraph_index` | UInt32 | yes | Paragraph within section |
| 12 | `element_type` | Utf8 | no | prose/table/figure/code/footnote/list |
| 13 | `namespace` | Utf8 | no | Partition key |
| 14 | `y_layer` | UInt8 | no | Always 0 (Y0) |
| 15 | `extracted_by` | Utf8 | yes | Agent/process that chunked |
| 16 | `created_at` | Timestamp(ms, UTC) | no | Creation timestamp |

### Schema Version Constants

- `TRIPLES_SCHEMA_VERSION = "1.1.0"` — written to Parquet file metadata on commit/snapshot
- `CHUNKS_SCHEMA_VERSION = "1.0.0"` — written to Parquet file metadata on chunk snapshot

### Migration

`normalize_to_current(batch, "1.0.0")` adds null `source_chunk_id` at index 9.

---

## v1.0.0 (2026-03-01) — Baseline

**VOY-142 (Arrow Substrate)**

- Triples: 15 columns (triple_id through deleted)
- Embeddings: 2 columns (entity_id, vector)
- Metadata: 5 columns (entity_id, y_layer, namespace, access_count, last_accessed)
