//! Durable ID high-water floor (CH-6058 / SG-6057).
//!
//! # Why
//!
//! ID allocation's only source of truth was the current store — `max + 1` for
//! items, and (worse) `base + count + 1` for proposals. Both are correct only
//! while the store is complete. **The store is exactly the thing that rolls
//! back.**
//!
//! 2026-07-19: after the HZ-6053 ENOSPC rollback the proposal store's highest
//! id was `PROP-3452`, while `origin/main` already had `PROP-3453` (CH-6052)
//! and `PROP-3454` (EX-5860) merged. The next `nk pr create` was handed
//! `PROP-3453` — two different PRs, one id, with a reviewer mid-review on it.
//! The next two creates would have collided as well.
//!
//! # The idea
//!
//! Git history is an append-only record of every id that has ever been merged:
//! commit subjects already carry `Merge PROP-3453 (CH-6052): ...`. That record
//! survives anything the store can do to itself. So:
//!
//! 1. Keep a small persisted integer — the highest id ever *handed out* — and
//!    raise it on every allocation. It is written separately from the Parquet
//!    and is a few bytes, so it survives a rollback (and is far likelier to
//!    land than a multi-megabyte save on a nearly-full disk).
//! 2. Seed it from git at startup, so even a wholesale restore of an old backup
//!    cannot drag the floor backwards.
//!
//! Allocation then takes `max(store_max, floor) + 1`. If the floor is ahead of
//! the store, that is itself a rollback signal worth logging.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Filename of the persisted floor, alongside the Parquet store.
pub const ID_FLOOR_FILE: &str = "_id_floor.json";

/// A monotonic high-water mark for one id namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdFloor {
    /// Highest proposal number ever handed out (the numeric part of `PROP-n`).
    pub proposals: usize,
    /// Highest work-item number ever handed out (EX/CH/VY/... share a counter).
    pub items: usize,
}

fn floor_path(data_dir: &Path) -> PathBuf {
    data_dir.join(ID_FLOOR_FILE)
}

/// Minimal JSON parse — avoids pulling serde_json into this crate for two ints.
fn parse_field(text: &str, key: &str) -> Option<usize> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    let colon = rest.find(':')? + 1;
    let digits: String = rest[colon..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

impl IdFloor {
    /// Load the floor, or a zero floor if absent/unreadable.
    ///
    /// A missing or corrupt floor is never an error: a zero floor simply means
    /// "no constraint", which is the previous behaviour. Failing startup over
    /// it would turn a safety net into an outage.
    pub fn load(data_dir: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(floor_path(data_dir)) else {
            return IdFloor::default();
        };
        IdFloor {
            proposals: parse_field(&text, "proposals").unwrap_or(0),
            items: parse_field(&text, "items").unwrap_or(0),
        }
    }

    /// Persist the floor. Written durably — this file is the thing that has to
    /// outlive a rollback, so a buffered write that never reaches disk defeats
    /// the entire mechanism.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = floor_path(data_dir);
        let tmp = path.with_extension("json.floor-tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            write!(
                f,
                "{{\"proposals\":{},\"items\":{}}}",
                self.proposals, self.items
            )?;
            f.flush()?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Raise the proposal floor. Monotonic — never moves backwards.
    ///
    /// Returns true if the floor actually advanced.
    pub fn raise_proposals(&mut self, n: usize) -> bool {
        if n > self.proposals {
            self.proposals = n;
            true
        } else {
            false
        }
    }

    /// Raise the item floor. Monotonic — never moves backwards.
    pub fn raise_items(&mut self, n: usize) -> bool {
        if n > self.items {
            self.items = n;
            true
        } else {
            false
        }
    }

    /// True when the floor is ahead of what the store believes — i.e. the store
    /// has lost ids it previously handed out. That is a rollback signature.
    pub fn store_looks_truncated(&self, store_max_proposals: usize) -> bool {
        self.proposals > store_max_proposals
    }
}

/// Scan git history for the highest `PROP-<n>` ever mentioned in a commit
/// subject, to seed the floor.
///
/// Deliberately reads only local history (`git log`) — no fetch, no network.
/// Merge subjects in this repo carry `Merge PROP-3453 (CH-6052): ...`, so the
/// ids of everything ever merged are recoverable even if every Parquet is lost.
///
/// Returns `None` if git is unavailable or nothing matches; callers treat that
/// as "no additional constraint" rather than an error.
pub fn git_high_water_proposals(repo_root: &Path) -> Option<usize> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["log", "--no-merges=false", "--format=%s", "-n", "5000"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut max = 0usize;
    for line in text.lines() {
        let mut rest = line;
        while let Some(pos) = rest.find("PROP-") {
            rest = &rest[pos + "PROP-".len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<usize>()
                && n > max
            {
                max = n;
            }
        }
    }
    (max > 0).then_some(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_floor_loads_as_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let floor = IdFloor::load(dir.path());
        assert_eq!(floor, IdFloor::default());
        assert_eq!(floor.proposals, 0);
    }

    #[test]
    fn floor_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut floor = IdFloor::default();
        floor.raise_proposals(3455);
        floor.raise_items(6058);
        floor.save(dir.path()).expect("save");

        let loaded = IdFloor::load(dir.path());
        assert_eq!(loaded.proposals, 3455);
        assert_eq!(loaded.items, 6058);
    }

    #[test]
    fn floor_is_monotonic() {
        let mut floor = IdFloor::default();
        assert!(floor.raise_proposals(3455));
        assert!(!floor.raise_proposals(3400), "must not move backwards");
        assert_eq!(floor.proposals, 3455);
        assert!(!floor.raise_proposals(3455), "equal is not a raise");
    }

    /// A corrupt floor must degrade to "no constraint", not break startup.
    #[test]
    fn corrupt_floor_degrades_to_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(ID_FLOOR_FILE), b"{not json at all").expect("write");
        let floor = IdFloor::load(dir.path());
        assert_eq!(floor.proposals, 0);
    }

    #[test]
    fn floor_save_leaves_no_tmp_litter() {
        let dir = tempfile::tempdir().expect("tempdir");
        IdFloor {
            proposals: 1,
            items: 2,
        }
        .save(dir.path())
        .expect("save");
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec![ID_FLOOR_FILE.to_string()]);
    }

    #[test]
    fn detects_a_truncated_store() {
        let mut floor = IdFloor::default();
        floor.raise_proposals(3455);
        assert!(
            floor.store_looks_truncated(3452),
            "floor ahead of store max = rollback signature"
        );
        assert!(!floor.store_looks_truncated(3455));
        assert!(!floor.store_looks_truncated(3460));
    }

    #[test]
    fn git_high_water_returns_none_outside_a_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(git_high_water_proposals(dir.path()).is_none());
    }
}
