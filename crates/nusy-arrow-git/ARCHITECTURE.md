# nusy-arrow-git Architecture

This document explains the internal design of nusy-arrow-git so that an architect
can understand the system without reading source code.

## Overview

nusy-arrow-git implements version control for in-memory Arrow knowledge graphs.
Instead of versioning files (like traditional Git), it versions **Arrow RecordBatches**
containing RDF-like triples, organized into namespace partitions.

```
                    ┌──────────────────────────────────────────────┐
                    │            GitObjectStore                    │
                    │  ┌────────────────────────────────────────┐  │
                    │  │         ArrowGraphStore                │  │
                    │  │  ┌─────────┐ ┌─────────┐ ┌──────────┐ │  │
                    │  │  │  world  │ │  work   │ │ research │ │  │
                    │  │  │ batches │ │ batches │ │ batches  │ │  │
                    │  │  └─────────┘ └─────────┘ └──────────┘ │  │
                    │  └────────────────────────────────────────┘  │
                    └──────────────┬──────────────┬───────────────┘
                                   │              │
                          commit() │              │ checkout()
                                   ▼              │
                    ┌──────────────────────────────┴───────────────┐
                    │          Parquet Snapshots                    │
                    │  snapshots/{commit_id}/                       │
                    │    ├── world.parquet                          │
                    │    ├── work.parquet                           │
                    │    ├── research.parquet                       │
                    │    └── self.parquet                           │
                    └──────────────────────────────────────────────┘
```

**Lifecycle:** Add triples to the in-memory graph → `commit()` writes each
namespace as a Parquet file under `snapshots/{commit_id}/` → `checkout()` clears
the in-memory graph and reloads from Parquet → `diff()` compares two commits by
loading both snapshots and computing set differences on triples.

## CommitsTable — The History DAG

The `CommitsTable` is an append-only, in-memory table that records every commit.

### Schema

| Column | Type | Description |
|--------|------|-------------|
| `commit_id` | Utf8 | UUID v4, generated at commit time |
| `parent_ids` | List\<Utf8\> | Zero parents (root), one (linear), or two (merge) |
| `timestamp` | Timestamp(ms, UTC) | Wall-clock time of commit |
| `message` | Utf8 | Human-readable commit message |
| `author` | Utf8 | Agent name (e.g., "DGX", "Mini", "M5") |

### DAG Structure

```
    c1 (root, parents=[])
    │
    c2 (parents=[c1])
   / \
  c3   c4 (branch — both have parent c2)
   \ /
    c5 (merge commit, parents=[c3, c4])
```

- **Root commits** have an empty `parent_ids` list.
- **Linear commits** have exactly one parent.
- **Merge commits** have exactly two parents.

### DAG Traversal

`log(start, limit)` performs depth-first traversal from a starting commit,
following parent pointers. Returns commits newest-first.

`ancestors(commit_id)` returns all reachable commits (BFS), including the
starting commit itself.

`find_common_ancestor(a, b)` performs BFS from both commits simultaneously and
returns the first overlap — the most recent common ancestor. This is the basis
for 3-way merge.

## RefsTable — Branches and Tags

The `RefsTable` manages named pointers to commits.

### Schema

| Column | Type | Description |
|--------|------|-------------|
| `ref_name` | Utf8 | Branch or tag name (e.g., "main", "v1.0") |
| `commit_id` | Utf8 | The commit this ref points to |
| `ref_type` | Utf8 | "branch" or "tag" |
| `is_head` | Boolean | Exactly one ref is HEAD at any time |
| `created_at` | Timestamp(ms) | When the ref was created |

### Branch vs Tag Semantics

| Property | Branch | Tag |
|----------|--------|-----|
| Mutable | Yes — `update_ref()` moves the pointer | No — immutable after creation |
| Deletable | Yes — `delete_branch()` (unless it's HEAD) | No |
| HEAD-eligible | Yes — `switch_head()` | No |

### HEAD Tracking

Exactly one branch is HEAD at all times. `switch_head()` atomically clears
the old HEAD flag and sets the new one. Attempting to delete the HEAD branch
returns `RefsError::DeleteHead`.

### Initialization

A new `RefsTable` starts empty. Call `init_main(commit_id)` after the first
commit to create the "main" branch pointing to it with `is_head = true`.

## WAL + Atomic Save

The `save()` subsystem provides crash-safe persistence for the entire state
(graph data, commits, refs) without creating a versioned commit.

### Algorithm

```
save(store, save_dir):
  1. mkdir -p save_dir
  2. Write _wal.json = ["world", "work", ...]   ← namespaces to save
  3. For each namespace with data:
     a. Write {ns}.parquet.tmp
     b. fs::rename({ns}.parquet.tmp → {ns}.parquet)  ← atomic on POSIX
  4. Write _commits.json.tmp → rename → _commits.json
  5. Write _refs.json.tmp → rename → _refs.json
  6. Remove _wal.json                            ← save complete
```

### Crash Recovery

| Crash Point | State on Disk | Recovery |
|-------------|---------------|----------|
| Before step 2 | No WAL, no changes | Nothing to recover |
| During step 3 | WAL exists, some .parquet are old, some new | Each .parquet is fully old or fully new (atomic rename). WAL cleaned on next restore. |
| After step 3, before step 6 | WAL exists, all .parquet are new | Data is current. WAL cleaned on next restore. |
| After step 6 | No WAL, all files current | Clean state |

The key insight: `fs::rename()` is atomic on POSIX. A Parquet file is never
partially written — it's either the old version or the new version.

### File Layout

```
save_dir/
  ├── world.parquet        # Namespace data (one per non-empty namespace)
  ├── work.parquet
  ├── research.parquet
  ├── self.parquet
  ├── _commits.json        # CommitsTable serialized as JSON array
  ├── _refs.json           # RefsTable serialized as JSON array
  └── _wal.json            # (transient — only exists during save)
```

### Advanced Save Options

`save_with_options()` supports:

- **Zstd compression:** Writes compressed Parquet files. Significantly smaller
  on disk (2-5x) with negligible CPU overhead.
- **Incremental save:** Only writes namespaces in the `dirty_namespaces` set,
  leaving other Parquet files untouched. Useful when only one namespace changed.

## 3-Way Merge

The merge algorithm detects semantic conflicts at the triple level.

### Algorithm

```
merge(store, commits, commit_a, commit_b, author):
  1. ancestor = find_common_ancestor(commits, commit_a, commit_b)
     → If none: return NoCommonAncestor
  2. diff_a = diff(ancestor → commit_a)   // What A changed
  3. diff_b = diff(ancestor → commit_b)   // What B changed
  4. Detect conflicts:
     For each (subject, predicate, namespace) that appears in BOTH
     diff_a.added AND diff_b.added with DIFFERENT object values:
       → Record as Conflict { subject, predicate, namespace, object_a, object_b }
  5. If conflicts and strategy = Manual:
     → return MergeResult::Conflict(conflicts)
  6. Otherwise, apply resolution:
     a. Checkout commit_a (start from A's state)
     b. Apply diff_b.added (add B's new triples)
     c. Apply diff_b.removed (remove triples B deleted)
     d. Resolve conflicts per strategy
     e. Create merge commit with parents=[commit_a, commit_b]
  7. return MergeResult::Clean(merge_commit)
```

### Conflict Example

```
Ancestor state:  alice → friendOf → bob
Branch A adds:   alice → friendOf → carol    (changed object)
Branch B adds:   alice → friendOf → dave     (changed object)

Conflict detected:
  subject:   "alice"
  predicate: "friendOf"
  namespace: "world"
  object_a:  "carol"
  object_b:  "dave"
```

### Resolution Strategies

| Strategy | Behavior |
|----------|----------|
| `Manual` | Returns `Vec<Conflict>` — caller must resolve each |
| `Ours` | Keeps A's object for all conflicts |
| `Theirs` | Keeps B's object for all conflicts |
| `LastWriterWins` | Compares `consolidated_at` timestamps; newer wins |
| `Custom(closure)` | Caller-provided function maps `&Conflict → Resolution` |

Each `Resolution` can be:
- `KeepOurs` — use A's value
- `KeepTheirs` — use B's value
- `KeepBoth` — add both as separate triples
- `Drop` — remove both values

### Non-Conflicting Changes

These are always applied cleanly:
- A adds a triple, B doesn't touch it → added
- B adds a triple, A doesn't touch it → added
- Both remove the same triple → removed
- A adds, B removes a different triple → both applied

## Schema Versioning

Parquet snapshots include schema version metadata for forward compatibility.

### How It Works

1. **On commit:** The current `TRIPLES_SCHEMA_VERSION` (from `nusy-arrow-core`)
   is written as Parquet file metadata under key `"nusy_schema_version"`.

2. **On checkout:** After reading a Parquet file, `normalize_to_current(batch, version)`
   transforms old-schema RecordBatches to the current schema. This handles:
   - Added columns (filled with defaults/nulls)
   - Renamed columns (mapped to new names)
   - Type changes (cast to current types)

3. **Default version:** If no `nusy_schema_version` metadata key exists (legacy
   files), the version defaults to `"1.0.0"`.

### Design Principles

- **Forward-compatible only.** Old code cannot read new schemas. New code reads
  all old schemas via normalization.
- **Version stamp, not migration.** Each Parquet file carries its version.
  There's no global migration step. Normalization happens at read time.
- **Schema changes tracked** in `SCHEMA-CHANGELOG.md` (when it exists).

## Namespace Partitioning

The graph store divides triples into four namespaces:

| Namespace | Purpose | Example Content |
|-----------|---------|-----------------|
| `world` | External knowledge | Domain ontologies, learned facts |
| `work` | Operational state | Kanban items, task metadata |
| `research` | Research artifacts | Hypotheses, experiment results |
| `self` | Being identity | Persona, calibration, journal |

### Storage Impact

Each namespace is stored as a separate Parquet file in both commit snapshots
and save points. This has two benefits:

1. **Independent I/O:** Saving or restoring a single namespace doesn't touch
   others. The incremental save feature (`dirty_namespaces`) exploits this.

2. **Isolation guarantees:** Operations within a namespace cannot accidentally
   modify another namespace's data. Cross-namespace relationships require
   explicit bridge triples.

### Diff Awareness

`DiffEntry` includes a `namespace` field. Diffs are computed across all
namespaces simultaneously, but each entry knows which namespace it belongs to.
This allows merge conflict resolution to be namespace-aware (e.g., "keep ours
for world, keep theirs for work").

## Cherry-Pick and Revert

### Cherry-Pick

Selectively applies one commit's changes onto a different HEAD:

```
cherry_pick(source_commit, head_commit):
  1. parent = source_commit.parent    (must have exactly 1)
  2. changes = diff(parent → source)  (what the source commit did)
  3. Check for conflicts with head:
     Any triple in changes.added that conflicts with head's state?
     → If yes: return CherryPickError::Conflict(count)
  4. Checkout head
  5. Apply changes.added, changes.removed
  6. Create new commit with head as parent
```

Cannot cherry-pick merge commits (multiple parents) or root commits (no parent).

### Revert

Creates an inverse commit that undoes a previous change:

```
revert(commit_to_undo, head_commit):
  1. parent = commit_to_undo.parent    (must have exactly 1)
  2. changes = diff(parent → commit)   (what the commit did)
  3. Checkout head
  4. Apply INVERSE: add what was removed, remove what was added
  5. Create new commit with head as parent, message: "Revert: {original}"
```

Cannot revert merge commits (ambiguous — which parent to diff against?).

## Module Dependency Graph

```
object_store ← commit ← checkout
                  ↑          ↑
                  │          │
               history    diff ← merge
                             ↑      ↑
                             │      │
                        cherry_pick │
                                    │
                              revert┘

save (independent — uses GitObjectStore directly)
refs (independent — manages branch/tag metadata)
```

- `commit` depends on `object_store` (reads namespace batches, writes Parquet)
- `checkout` depends on `commit` (reads Parquet back into store)
- `diff` depends on `checkout` (loads two commits to compare)
- `merge` depends on `diff` + `history` (finds ancestor, diffs both branches)
- `cherry_pick` depends on `diff` + `checkout` (diffs source, applies to head)
- `revert` depends on `diff` + `checkout` (diffs commit, applies inverse)
- `save` is independent (raw namespace persistence, no commit semantics)
- `refs` is independent (branch/tag metadata, no graph data)
