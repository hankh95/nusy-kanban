//! nusy-arrow-git — Graph-native git primitives for NuSy Arrow substrate.
//!
//! Provides 8 git primitives operating on Arrow RecordBatches, designed
//! for versioning RDF-like knowledge graphs stored in `nusy-arrow-core`.
//!
//! # Primitives
//!
//! 1. **Object Store** — the live [`GitObjectStore`] wrapping `ArrowGraphStore`
//! 2. **Commit** — snapshot state to Parquet + record in [`CommitsTable`]
//! 3. **Checkout** — restore state from a commit's Parquet snapshot
//! 4. **History DAG** — traverse parent_ids for [`log`] and [`ancestors`]
//! 5. **Branch/Head (Refs)** — lightweight branch pointers via [`RefsTable`]
//! 6. **Diff** — object-level comparison between commits via [`diff()`]
//! 7. **Merge** — 3-way merge with conflict detection/resolution via [`merge()`]
//! 8. **Save** — crash-safe persistence without creating a commit via [`save()`]
//!
//! # Performance (DGX Spark, 10K triples)
//!
//! | Operation | Measured | Gate |
//! |-----------|----------|------|
//! | Commit | ~3ms | < 25ms |
//! | Checkout | ~1.3ms | < 25ms |
//! | Commit+Checkout (H-GIT-1) | ~4.3ms | < 50ms |
//! | Save+Restore (M-SAVE) | ~4.3ms | < 100ms |
//! | Awakening (M-119) | ~1.3ms | < 200ms |
//! | Batch add 10K | ~4.3ms | < 10ms |
//!
//! # Merge Conflict Resolution
//!
//! The [`merge()`] function detects conflicts (same subject+predicate with
//! different objects across branches). Use [`merge_with_strategy`] for
//! automatic resolution via [`MergeStrategy::Ours`], [`MergeStrategy::Theirs`],
//! [`MergeStrategy::LastWriterWins`], or a custom closure.
//!
//! # Crash-Safe Persistence
//!
//! [`save()`] uses a WAL marker + atomic file rename pattern. If a save is
//! interrupted, the previous Parquet files remain valid. [`save_with_options`]
//! adds zstd compression and incremental saves (only dirty namespaces).

pub mod blame;
pub mod checkout;
pub mod cherry_pick;
pub mod commit;
pub mod diff;
pub mod history;
pub mod merge;
pub mod object_store;
pub mod rebase;
pub mod refs;
pub mod remote;
pub mod revert;
pub mod save;

pub use blame::{BlameEntry, blame};
pub use checkout::checkout;
pub use cherry_pick::cherry_pick;
pub use commit::{Commit, CommitsTable, create_commit};
pub use diff::{DiffEntry, DiffResult, diff, diff_nondestructive};
pub use history::{ancestors, find_common_ancestor, log};
pub use merge::{Conflict, MergeResult, MergeStrategy, Resolution, merge, merge_with_strategy};
pub use object_store::{GitConfig, GitObjectStore};
pub use rebase::{RebaseResult, rebase};
pub use refs::{Ref, RefType, RefsTable};
pub use remote::{
    RemoteError, Snapshot, bytes_to_snapshot, restore_snapshot, snapshot_state, snapshot_to_bytes,
};
pub use revert::revert;
pub use save::{
    SaveMetrics, SaveOptions, persist_commits, restore, restore_commits, restore_full,
    restore_named_batches, save, save_full, save_named_batches, save_with_options,
};
