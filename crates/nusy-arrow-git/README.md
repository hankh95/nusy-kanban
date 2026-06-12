# nusy-arrow-git

[![Crates.io](https://img.shields.io/crates/v/nusy-arrow-git.svg)](https://crates.io/crates/nusy-arrow-git)
[![docs.rs](https://img.shields.io/docsrs/nusy-arrow-git)](https://docs.rs/nusy-arrow-git)
[![CI](https://github.com/hankh95/nusy-product-team/actions/workflows/ci.yml/badge.svg)](https://github.com/hankh95/nusy-product-team/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Graph-native Git for Arrow RecordBatches — version, branch, merge, and diff
knowledge graphs at sub-5ms latency.**

## Motivation

Traditional Git versions *files*. But when your data is a knowledge graph —
millions of RDF-like triples stored as Arrow RecordBatches — serializing to
files and back is a wasteful detour.

**nusy-arrow-git** operates directly on in-memory Arrow tables. Commits snapshot
namespace-partitioned RecordBatches to Parquet. Checkouts restore them. Diffs
compare triples, not lines. Merges detect semantic conflicts (same subject +
predicate, different objects), not textual ones.

The result: version control that understands your data's structure, with
commit+checkout cycles completing in under 5ms for 10,000 triples.

## Features

Twelve git primitives, all operating on Arrow RecordBatches:

| Primitive | Description |
|-----------|-------------|
| **commit** | Snapshot graph state to per-namespace Parquet files |
| **checkout** | Restore graph state from a commit's snapshots |
| **history** | DAG traversal — `log`, `ancestors`, `find_common_ancestor` |
| **refs** | Mutable branches + immutable tags via `RefsTable` |
| **diff** | Object-level comparison — added/removed triples with full provenance |
| **merge** | 3-way merge with pluggable conflict resolution strategies |
| **save** | Crash-safe persistence via WAL + atomic Parquet writes |
| **cherry_pick** | Apply a single commit's changes onto a different HEAD |
| **revert** | Create an inverse commit that undoes a previous change |
| **delete_branch** | Remove a branch ref (cannot delete HEAD) |
| **tags** | Immutable ref pointers for release marking |
| **object_store** | `GitObjectStore` wrapper with configurable snapshot directory |

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
nusy-arrow-git = "0.14"
nusy-arrow-core = "0.14"
```

Create a store, add triples, commit, checkout, and diff:

```rust
use nusy_arrow_core::{Namespace, Triple, YLayer};
use nusy_arrow_git::{
    CommitsTable, GitObjectStore, checkout, create_commit, diff,
};

fn triple(s: &str, p: &str, o: &str) -> Triple {
    Triple {
        subject: s.into(), predicate: p.into(), object: o.into(),
        graph: None, confidence: None, source_document: None,
        source_chunk_id: None, extracted_by: None, caused_by: None,
        derived_from: None, consolidated_at: None,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a git-aware graph store
    let mut obj = GitObjectStore::with_snapshot_dir("my_snapshots");
    let mut commits = CommitsTable::new();

    // Add a triple and commit
    obj.store.add_triple(&triple("alice", "knows", "bob"), Namespace::World, YLayer::Semantic)?;
    let c1 = create_commit(&obj, &mut commits, vec![], "Add alice→bob", "dev")?;

    // Add another triple and commit
    obj.store.add_triple(&triple("bob", "knows", "carol"), Namespace::World, YLayer::Semantic)?;
    let c2 = create_commit(&obj, &mut commits, vec![c1.commit_id.clone()], "Add bob→carol", "dev")?;

    // Diff between commits
    let changes = diff(&mut obj, &commits, &c1.commit_id, &c2.commit_id)?;
    println!("Added: {}, Removed: {}", changes.added.len(), changes.removed.len());

    // Checkout the first commit (restores exact state)
    checkout(&mut obj, &commits, &c1.commit_id)?;
    Ok(())
}
```

## Installation

**Minimum Supported Rust Version (MSRV):** Rust 2024 edition

**As a workspace dependency:**

```toml
# Root Cargo.toml
[workspace.dependencies]
nusy-arrow-git = { path = "crates/nusy-arrow-git" }
```

**Dependencies:** Relies on `nusy-arrow-core` for the `ArrowGraphStore`, `Triple`,
`Namespace`, and `YLayer` types. Uses Apache Arrow 55+ and Parquet 55+ for
columnar storage.

## Core Concepts

### "There Are No Files"

Arrow tables ARE the data. Parquet snapshots are the undo log. There is no
serialize-to-text step, no line-based diffing, no merge conflict markers in files.

When you `commit()`, each namespace (world, work, research, self) is written as
a separate Parquet file. When you `checkout()`, those Parquet files are loaded
back into Arrow RecordBatches. The graph store's in-memory state is always the
source of truth; Parquet is the persistence layer.

```
┌─────────────┐   commit()    ┌───────────────────────┐
│ ArrowGraph  │──────────────→│ snapshots/{id}/        │
│ Store       │               │   world.parquet        │
│ (in-memory) │←──────────────│   work.parquet         │
└─────────────┘   checkout()  │   research.parquet     │
                              └───────────────────────┘
```

### WAL Crash Safety

The `save()` function uses a write-ahead log pattern:

1. Write `_wal.json` listing namespaces to be saved
2. Write each namespace to `{name}.parquet.tmp`
3. Atomic `rename()` from `.tmp` to `.parquet`
4. Remove `_wal.json`

If the process crashes mid-save, the WAL marker tells the next `restore()` that
the previous save was interrupted. Because writes use atomic rename, each Parquet
file is either fully old or fully new — never partial.

### 3-Way Merge with Conflict Detection

Merges find the common ancestor (via BFS on the commit DAG), then diff both
branches against it. A **conflict** occurs when two branches modify the same
`(subject, predicate, namespace)` tuple with different object values.

Four built-in resolution strategies:

| Strategy | Behavior |
|----------|----------|
| `Manual` | Return conflicts for caller to resolve |
| `Ours` | Always keep branch A's value |
| `Theirs` | Always keep branch B's value |
| `LastWriterWins` | Compare `consolidated_at` timestamps |

Custom strategies are supported via closure:

```rust
use nusy_arrow_git::{MergeStrategy, Resolution};

let strategy = MergeStrategy::Custom(Box::new(|conflict| {
    if conflict.namespace == "world" {
        Resolution::KeepOurs
    } else {
        Resolution::KeepTheirs
    }
}));
```

## Performance

Benchmarked on DGX Spark with 10,000 triples:

| Operation | Target | Measured |
|-----------|--------|----------|
| Commit + Checkout | < 50ms | ~4.3ms |
| Save + Restore | < 100ms | ~4.3ms |
| Being awakening (M-119) | < 200ms | ~50ms |
| Batch add 10K triples | < 10ms | ~4.3ms |
| Single commit | < 25ms | ~3ms |
| Single checkout | < 25ms | ~1.3ms |

All operations are well under their performance gates, leaving headroom for
graphs 10-100x larger.

## Safety and Invariants

- **CommitsTable is append-only.** History is never rewritten. Even `revert()`
  creates a new commit with inverse changes.
- **WAL guarantees.** A crash during `save()` cannot corrupt existing Parquet
  files. Atomic rename ensures each file is fully old or fully new.
- **Tag immutability.** Once created, a tag's commit pointer cannot be changed
  or deleted. Use `delete_branch()` for mutable refs.
- **HEAD protection.** The currently checked-out branch cannot be deleted.
- **Merge commits record both parents.** The commit DAG is a true DAG, not a
  linear chain.
- **Schema versioning.** Parquet files include a `nusy_schema_version` metadata
  key. On checkout, `normalize_to_current()` adapts old schemas forward.
- **Thread safety.** Individual operations are not internally synchronized.
  Callers must ensure exclusive access to `GitObjectStore` during mutations.

## Panics

- `create_commit` will return `CommitError::Io` if the snapshot directory cannot
  be created (permissions, disk full).
- `checkout` will return `CommitError::NotFound` if the commit's snapshot
  directory is missing (e.g., manually deleted).
- `revert` returns `RevertError::MergeCommit` for merge commits (multiple parents).

No function in this crate calls `panic!()` or `unwrap()` in library code.

## See Also

- **[nusy-arrow-core](../nusy-arrow-core/)** — The Arrow graph store, triple schema,
  namespace partitioning, and Y-layer classification that nusy-arrow-git operates on.
- **[nusy-kanban](../nusy-kanban/)** — Arrow-native kanban CLI that uses
  `save_named_batches()` / `restore_named_batches()` for crash-safe persistence.
  A real-world consumer of this crate's save primitives.
- **[nusy-graph-review](../nusy-graph-review/)** — Graph-native code review built
  on top of nusy-arrow-git's diff and merge primitives.

## License

MIT — see [LICENSE](../../LICENSE) for details.
