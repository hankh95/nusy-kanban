# nusy-arrow-git API Reference

Every public function, struct, and enum grouped by module.

---

## Object Store

### `GitObjectStore`

The git-aware wrapper around `ArrowGraphStore`.

```rust
pub struct GitObjectStore {
    pub store: ArrowGraphStore,
    pub config: GitConfig,
}
```

| Method | Signature | Description |
|--------|-----------|-------------|
| `new()` | `fn new() -> Self` | Create with default config (`.nusy-arrow/snapshots`) |
| `with_snapshot_dir()` | `fn with_snapshot_dir(dir: impl Into<PathBuf>) -> Self` | Create with custom snapshot directory |
| `commit_snapshot_dir()` | `fn commit_snapshot_dir(&self, commit_id: &str) -> PathBuf` | Path to a commit's snapshot directory |
| `namespace_parquet_path()` | `fn namespace_parquet_path(&self, commit_id: &str, namespace: &str) -> PathBuf` | Path to a specific namespace Parquet file |

```rust
use nusy_arrow_git::GitObjectStore;

let store = GitObjectStore::with_snapshot_dir("/tmp/my_snapshots");
let path = store.commit_snapshot_dir("abc123");
// → /tmp/my_snapshots/abc123/
```

### `GitConfig`

```rust
pub struct GitConfig {
    pub snapshot_dir: PathBuf,
}
```

| Method | Signature | Description |
|--------|-----------|-------------|
| `new()` | `fn new(snapshot_dir: impl Into<PathBuf>) -> Self` | Create with custom path |
| `default()` | `fn default() -> Self` | Default: `.nusy-arrow/snapshots` |

---

## Commit

### `create_commit()`

Snapshot current graph state to Parquet and record in CommitsTable.

```rust
pub fn create_commit(
    obj_store: &GitObjectStore,
    commits_table: &mut CommitsTable,
    parent_ids: Vec<String>,
    message: &str,
    author: &str,
) -> Result<Commit, CommitError>
```

Writes each non-empty namespace as `{snapshot_dir}/{commit_id}/{namespace}.parquet`.
The commit_id is a UUID v4 generated internally. Parquet metadata includes
`nusy_schema_version` for forward compatibility.

```rust
use nusy_arrow_git::{GitObjectStore, CommitsTable, create_commit};

let obj = GitObjectStore::with_snapshot_dir("/tmp/snap");
let mut commits = CommitsTable::new();

// Root commit (no parents)
let c1 = create_commit(&obj, &mut commits, vec![], "initial", "Mini")?;

// Child commit
let c2 = create_commit(
    &obj, &mut commits,
    vec![c1.commit_id.clone()],
    "add triples", "Mini"
)?;
```

### `Commit`

```rust
#[derive(Debug, Clone)]
pub struct Commit {
    pub commit_id: String,       // UUID v4
    pub parent_ids: Vec<String>, // 0=root, 1=linear, 2=merge
    pub timestamp_ms: i64,       // Unix epoch milliseconds
    pub message: String,
    pub author: String,
}
```

### `CommitsTable`

Append-only history table.

| Method | Signature | Description |
|--------|-----------|-------------|
| `new()` | `fn new() -> Self` | Empty table |
| `append()` | `fn append(&mut self, commit: Commit)` | Add a commit record |
| `get()` | `fn get(&self, commit_id: &str) -> Option<&Commit>` | Look up by ID |
| `all()` | `fn all() -> &[Commit]` | All commits in insertion order |
| `len()` | `fn len() -> usize` | Number of commits |
| `is_empty()` | `fn is_empty() -> bool` | True if no commits |
| `to_record_batch()` | `fn to_record_batch() -> Result<RecordBatch>` | Serialize to Arrow |

### `CommitError`

```rust
pub enum CommitError {
    Arrow(arrow::error::ArrowError),
    Parquet(parquet::errors::ParquetError),
    Io(std::io::Error),
    NotFound(String),  // Commit snapshot directory missing
}
```

---

## Checkout

### `checkout()`

Restore graph state from a commit's Parquet snapshots.

```rust
pub fn checkout(
    obj_store: &mut GitObjectStore,
    commits_table: &CommitsTable,
    commit_id: &str,
) -> Result<(), CommitError>
```

**Clears the in-memory store**, then loads each namespace's Parquet file.
Applies `normalize_to_current()` for schema evolution.

**Warning:** Destroys any uncommitted changes in the store.

```rust
use nusy_arrow_git::{GitObjectStore, CommitsTable, checkout};

checkout(&mut obj, &commits, &c1.commit_id)?;
// Store now contains exactly what was committed in c1
```

**Performance:** ~1.3ms for 10K triples (DGX Spark).

---

## Diff

### `diff()`

Object-level comparison between two commits.

```rust
pub fn diff(
    obj_store: &mut GitObjectStore,
    commits_table: &CommitsTable,
    base_commit_id: &str,
    head_commit_id: &str,
) -> Result<DiffResult, CommitError>
```

**Warning:** Mutates the store (calls `checkout()` internally). Use
`diff_nondestructive()` to preserve uncommitted changes.

### `diff_nondestructive()`

Same as `diff()` but saves and restores the store's current state.

```rust
pub fn diff_nondestructive(
    obj_store: &mut GitObjectStore,
    commits_table: &CommitsTable,
    base_commit_id: &str,
    head_commit_id: &str,
) -> Result<DiffResult, CommitError>
```

### `DiffResult`

```rust
pub struct DiffResult {
    pub added: Vec<DiffEntry>,    // In head, not in base
    pub removed: Vec<DiffEntry>,  // In base, not in head
}

impl DiffResult {
    pub fn is_empty(&self) -> bool       // No changes
    pub fn total_changes(&self) -> usize // added.len() + removed.len()
}
```

### `DiffEntry`

A single added or removed triple with full provenance metadata.

```rust
pub struct DiffEntry {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub namespace: String,
    pub y_layer: u8,
    pub confidence: Option<f64>,
    pub graph: Option<String>,
    pub source_document: Option<String>,
    pub source_chunk_id: Option<String>,
    pub caused_by: Option<String>,
    pub derived_from: Option<String>,
    pub consolidated_at: Option<i64>,
}
```

**Triple identity:** `(subject, predicate, object, namespace)` — all four
fields must match for two triples to be considered the same.

```rust
use nusy_arrow_git::diff;

let changes = diff(&mut obj, &commits, &c1.commit_id, &c2.commit_id)?;
for entry in &changes.added {
    println!("{} → {} → {} ({})",
        entry.subject, entry.predicate, entry.object, entry.namespace);
}
println!("Total: {} changes", changes.total_changes());
```

**Performance:** O(n) in total triples across both commits. Each commit
is loaded and compared as sorted triple sets.

---

## Merge

### `merge()`

3-way merge with `Manual` strategy (returns conflicts without resolving).

```rust
pub fn merge(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    commit_a_id: &str,
    commit_b_id: &str,
    author: &str,
) -> Result<MergeResult, MergeError>
```

### `merge_with_strategy()`

3-way merge with configurable conflict resolution.

```rust
pub fn merge_with_strategy(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    commit_a_id: &str,
    commit_b_id: &str,
    author: &str,
    strategy: &MergeStrategy,
) -> Result<MergeResult, MergeError>
```

### `MergeResult`

```rust
pub enum MergeResult {
    Clean(Commit),              // Merge succeeded, new merge commit created
    Conflict(Vec<Conflict>),    // Conflicts detected (Manual strategy)
    NoCommonAncestor,           // No shared history
}
```

### `Conflict`

```rust
pub struct Conflict {
    pub subject: String,
    pub predicate: String,
    pub namespace: String,
    pub object_a: String,       // Value from branch A
    pub object_b: String,       // Value from branch B
}
```

### `MergeStrategy`

```rust
pub enum MergeStrategy {
    Manual,                                          // Return conflicts
    Ours,                                            // Always keep A
    Theirs,                                          // Always keep B
    LastWriterWins,                                   // Compare consolidated_at
    Custom(Box<dyn Fn(&Conflict) -> Resolution>),    // Caller-defined
}
```

### `Resolution`

```rust
pub enum Resolution {
    KeepOurs,    // Use branch A's object
    KeepTheirs,  // Use branch B's object
    KeepBoth,    // Add both as separate triples
    Drop,        // Remove both values
}
```

### `MergeError`

```rust
pub enum MergeError {
    Commit(CommitError),
    Store(nusy_arrow_core::StoreError),
}
```

```rust
use nusy_arrow_git::{merge_with_strategy, MergeStrategy, MergeResult, Resolution};

let result = merge_with_strategy(
    &mut obj, &mut commits,
    &branch_a_head, &branch_b_head,
    "Mini",
    &MergeStrategy::Custom(Box::new(|c| {
        if c.namespace == "world" { Resolution::KeepOurs }
        else { Resolution::KeepTheirs }
    })),
)?;

match result {
    MergeResult::Clean(commit) => println!("Merged: {}", commit.commit_id),
    MergeResult::Conflict(conflicts) => {
        for c in &conflicts {
            println!("CONFLICT: {}.{} — '{}' vs '{}'",
                c.subject, c.predicate, c.object_a, c.object_b);
        }
    }
    MergeResult::NoCommonAncestor => println!("No shared history"),
}
```

---

## History

### `log()`

Walk the commit DAG from a starting point, newest-first.

```rust
pub fn log<'a>(
    commits_table: &'a CommitsTable,
    start_commit_id: &str,
    limit: usize,  // 0 = unlimited
) -> Vec<&'a Commit>
```

```rust
use nusy_arrow_git::log;

for commit in log(&commits, &head_id, 10) {
    println!("{}: {} (by {})", commit.commit_id, commit.message, commit.author);
}
```

### `ancestors()`

All reachable commits from a starting point (BFS).

```rust
pub fn ancestors<'a>(
    commits_table: &'a CommitsTable,
    commit_id: &str,
) -> Vec<&'a Commit>
```

### `find_common_ancestor()`

Most recent common ancestor of two commits (for 3-way merge).

```rust
pub fn find_common_ancestor<'a>(
    commits_table: &'a CommitsTable,
    commit_a: &str,
    commit_b: &str,
) -> Option<&'a Commit>
```

Returns `None` if the commits have no shared history (disconnected DAGs).

---

## Refs (Branches & Tags)

### `RefsTable`

| Method | Signature | Description |
|--------|-----------|-------------|
| `new()` | `fn new() -> Self` | Empty table |
| `init_main()` | `fn init_main(&mut self, commit_id: &str)` | Create "main" as HEAD |
| `head()` | `fn head() -> Option<&Ref>` | Get current HEAD ref |
| `get()` | `fn get(&self, name: &str) -> Option<&Ref>` | Look up ref by name |
| `resolve()` | `fn resolve(&self, name: &str) -> Option<&str>` | Name → commit_id |
| `create_branch()` | `fn create_branch(&mut self, name: &str, commit_id: &str) -> Result<(), RefsError>` | Create new branch |
| `switch_head()` | `fn switch_head(&mut self, name: &str) -> Result<(), RefsError>` | Change HEAD to branch |
| `update_ref()` | `fn update_ref(&mut self, name: &str, commit_id: &str) -> Result<(), RefsError>` | Move branch pointer (not tags) |
| `delete_branch()` | `fn delete_branch(&mut self, name: &str) -> Result<(), RefsError>` | Delete (cannot delete HEAD) |
| `create_tag()` | `fn create_tag(&mut self, name: &str, commit_id: &str) -> Result<(), RefsError>` | Create immutable tag |
| `tags()` | `fn tags() -> Vec<&Ref>` | All tags |
| `branches()` | `fn branches() -> Vec<&Ref>` | All branches |
| `to_record_batch()` | `fn to_record_batch() -> Result<RecordBatch>` | Serialize to Arrow |

### `Ref`

```rust
#[derive(Debug, Clone)]
pub struct Ref {
    pub ref_name: String,
    pub commit_id: String,
    pub ref_type: RefType,
    pub is_head: bool,
    pub created_at_ms: i64,
}
```

### `RefType`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefType {
    Branch,
    Tag,
}

impl RefType {
    pub fn as_str(&self) -> &'static str  // "branch" or "tag"
}
```

### `RefsError`

```rust
pub enum RefsError {
    RefExists(String),       // Branch/tag with this name already exists
    RefNotFound(String),     // No ref with this name
    DeleteHead(String),      // Cannot delete the HEAD branch
    TagImmutable(String),    // Cannot update or delete a tag
    NotABranch(String),      // Attempted branch operation on a tag
}
```

```rust
use nusy_arrow_git::RefsTable;

let mut refs = RefsTable::new();
refs.init_main(&c1.commit_id);
refs.create_branch("feature", &c1.commit_id)?;
refs.switch_head("feature")?;

// After committing on feature:
refs.update_ref("feature", &c2.commit_id)?;

// Create release tag
refs.create_tag("v1.0", &c2.commit_id)?;
```

---

## Cherry-Pick

### `cherry_pick()`

Apply a single commit's changes onto a different HEAD.

```rust
pub fn cherry_pick(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    source_commit_id: &str,
    head_commit_id: &str,
    author: &str,
) -> Result<String, CherryPickError>  // Returns new commit_id
```

The source commit must have exactly one parent (not a merge or root commit).
If the cherry-picked changes conflict with HEAD, returns
`CherryPickError::Conflict(count)`.

### `CherryPickError`

```rust
pub enum CherryPickError {
    Commit(CommitError),
    Store(nusy_arrow_core::StoreError),
    NoParent(String),        // Source commit has no parent (root commit)
    Conflict(usize),         // N triples conflict with HEAD
}
```

```rust
use nusy_arrow_git::cherry_pick;

let new_id = cherry_pick(
    &mut obj, &mut commits,
    &source_commit_id,
    &current_head_id,
    "Mini",
)?;
println!("Cherry-picked as: {}", new_id);
```

---

## Revert

### `revert()`

Create an inverse commit that undoes a previous change.

```rust
pub fn revert(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    commit_id: &str,        // The commit to undo
    head_commit_id: &str,   // Current HEAD
    author: &str,
) -> Result<String, RevertError>  // Returns new revert commit_id
```

The target commit must have exactly one parent (cannot revert merge commits).
Creates a new commit that applies the inverse diff: adds what was removed,
removes what was added.

### `RevertError`

```rust
pub enum RevertError {
    Commit(CommitError),
    Store(nusy_arrow_core::StoreError),
    MergeCommit(String, usize),  // Cannot revert merge (has N parents)
    NoParent(String),            // Cannot revert root commit
}
```

```rust
use nusy_arrow_git::revert;

let revert_id = revert(
    &mut obj, &mut commits,
    &bad_commit_id,
    &current_head_id,
    "Mini",
)?;
println!("Reverted as: {}", revert_id);
```

---

## Save / Restore

### Core Functions

| Function | Description |
|----------|-------------|
| `save(store, dir)` | Save graph data only (no commits/refs) |
| `save_full(store, commits?, refs?, dir)` | Save everything |
| `restore(store, dir)` | Restore graph data only |
| `restore_full(store, dir)` | Restore everything, returns `(Option<CommitsTable>, Option<RefsTable>)` |
| `save_with_options(store, commits?, refs?, dir, opts)` | Save with compression/incremental, returns `SaveMetrics` |

### Generic Named Batches

For persisting arbitrary Arrow data (used by nusy-kanban):

| Function | Description |
|----------|-------------|
| `save_named_batches(entries, dir)` | Save `[(name, batches, schema)]` with WAL + atomic write |
| `restore_named_batches(dir, names)` | Restore named datasets, returns `Vec<(name, batches)>` |

### Commits-Only Persistence

| Function | Description |
|----------|-------------|
| `persist_commits(table, dir)` | Save CommitsTable as `_commits.json` |
| `restore_commits(dir)` | Load CommitsTable, returns `None` on first run |

### `SaveMetrics`

```rust
pub struct SaveMetrics {
    pub namespaces_saved: Vec<String>,
    pub bytes_written: u64,
    pub duration_ms: u128,
    pub compressed: bool,
}
```

### `SaveOptions`

```rust
pub struct SaveOptions {
    pub compress: bool,                            // Use zstd Parquet compression
    pub dirty_namespaces: Option<HashSet<Namespace>>, // Incremental save
}
```

### `SaveError`

```rust
pub enum SaveError {
    Io(std::io::Error),
    Parquet(parquet::errors::ParquetError),
    Arrow(arrow::error::ArrowError),
    NotFound(String),       // Save directory doesn't exist
    IncompleteWal,          // WAL marker found (previous save interrupted)
}
```

```rust
use nusy_arrow_git::{save_full, restore_full, save_with_options, SaveOptions};
use std::collections::HashSet;
use nusy_arrow_core::Namespace;

// Full save with commits and refs
save_full(&obj, Some(&commits), Some(&refs), Path::new("state/"))?;

// Incremental save with compression
let mut dirty = HashSet::new();
dirty.insert(Namespace::World);
let metrics = save_with_options(
    &obj, Some(&commits), Some(&refs),
    Path::new("state/"),
    &SaveOptions { compress: true, dirty_namespaces: Some(dirty) },
)?;
println!("Saved {} bytes in {}ms", metrics.bytes_written, metrics.duration_ms);

// Restore from crash
let (commits, refs) = restore_full(&mut obj, Path::new("state/"))?;
```
