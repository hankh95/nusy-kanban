//! Write-durability gate (CH-6056 / HZ-6053).
//!
//! # Why this exists
//!
//! The server used to *warn and continue* when it could not persist:
//!
//! ```text
//! Warning: failed to persist store after pr.merge: No space left on device
//! ```
//!
//! ...while still returning success to the client. During HZ-6053 that ran for
//! days. Agents were told their `create`/`move`/`merge` had succeeded, the
//! server served the correct state from memory, and the divergence stayed
//! invisible until a restart silently reverted the board by two months.
//!
//! **Acknowledging a write you could not durably store is the defect.** For a
//! fleet-wide source of truth, refusing writes is far safer than accepting them
//! into a buffer a restart will discard.
//!
//! # Design
//!
//! - Mutations are admitted *before* they touch memory. Once degraded, they are
//!   refused up front rather than applied-then-regretted.
//! - A persist failure flips the gate to `Degraded` **and** turns that specific
//!   response into an error, so the client is never told a lost write landed.
//! - Reads keep working. A degraded board is still worth reading, and going
//!   fully dark would be its own outage.
//! - Recovery is automatic: while degraded, a throttled canary write probes the
//!   store, and the gate returns to `Healthy` as soon as one succeeds.
//!
//! # Why a canary write rather than a free-space threshold
//!
//! A byte threshold is a proxy. The canary tests the actual property we care
//! about — *can this directory accept a write right now* — and so also catches
//! a read-only mount, a permissions change, or a full inode table, none of
//! which a free-space number would reveal. It needs no new dependency.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Resolve a data ROOT (what `--data-dir` points at) to the directory the
/// Parquet actually lives in.
///
/// These are not the same path — the store is `<root>/.nusy-kanban` — and
/// probing the root instead of the store is a silent false-negative: the root
/// can be perfectly writable while the store directory is on a full volume, is
/// mounted read-only, or has had its permissions changed. Falls back to the
/// root if the store dir cannot be resolved (which is itself a bad sign, and
/// the probe there will fail honestly).
pub fn store_dir(root: &Path) -> PathBuf {
    nusy_kanban::persist::data_dir(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Default bytes written by the canary probe.
///
/// Deliberately larger than a single page: a nearly-full filesystem will often
/// still accept a few bytes, so a tiny probe reports healthy right up until the
/// real save fails. This asks for enough headroom to be meaningful while
/// staying cheap.
pub const DEFAULT_PROBE_BYTES: usize = 64 * 1024;

/// How often to re-probe while degraded. Fast enough that recovery is prompt,
/// slow enough that a wedged store is not hammered once per request.
pub const DEFAULT_PROBE_INTERVAL: Duration = Duration::from_secs(10);

/// Whether the store is accepting durable writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Writes are landing on disk.
    Healthy,
    /// Writes are NOT durable — mutations are refused.
    Degraded {
        /// Human-readable cause, surfaced to clients.
        reason: String,
        /// When the gate degraded (millis since epoch).
        since_ms: u64,
    },
}

/// Admission gate for mutating commands.
#[derive(Debug)]
pub struct HealthGate {
    health: Health,
    last_probe_ms: u64,
    probe_interval: Duration,
    probe_bytes: usize,
}

impl Default for HealthGate {
    fn default() -> Self {
        Self::new()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl HealthGate {
    pub fn new() -> Self {
        HealthGate {
            health: Health::Healthy,
            last_probe_ms: 0,
            probe_interval: DEFAULT_PROBE_INTERVAL,
            probe_bytes: DEFAULT_PROBE_BYTES,
        }
    }

    /// Construct with explicit probe tuning (tests, and future config).
    pub fn with_probe(probe_bytes: usize, probe_interval: Duration) -> Self {
        HealthGate {
            health: Health::Healthy,
            last_probe_ms: 0,
            probe_interval,
            probe_bytes,
        }
    }

    pub fn health(&self) -> &Health {
        &self.health
    }

    pub fn is_degraded(&self) -> bool {
        matches!(self.health, Health::Degraded { .. })
    }

    pub fn reason(&self) -> Option<&str> {
        match &self.health {
            Health::Degraded { reason, .. } => Some(reason.as_str()),
            Health::Healthy => None,
        }
    }

    /// Write-and-remove a canary file. `Ok(())` means the store took a write.
    pub fn probe(data_dir: &Path, probe_bytes: usize) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join("_probe.tmp");
        let result = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::create(&path)?;
            let buf = vec![0u8; probe_bytes];
            f.write_all(&buf)?;
            // Without the flush/sync, a short write can be buffered and the
            // ENOSPC only surfaces at close — after we have already decided
            // the store is healthy.
            f.flush()?;
            f.sync_all()?;
            Ok(())
        })();
        // Always clean up, even on failure — a partial canary must not become
        // the orphaned .tmp that CH-6055 then quarantines.
        let _ = std::fs::remove_file(&path);
        result
    }

    /// Force a probe now and set health from the result. Used at startup so the
    /// server never comes up claiming healthy on a full disk.
    pub fn probe_now(&mut self, data_dir: &Path) {
        match Self::probe(data_dir, self.probe_bytes) {
            Ok(()) => self.recover(),
            Err(e) => self.degrade(format!("store is not writable: {e}")),
        }
        self.last_probe_ms = now_ms();
    }

    /// Decide whether a mutating command may proceed.
    ///
    /// Called BEFORE the handler runs, so a refused mutation never touches
    /// in-memory state — there is nothing to roll back.
    pub fn admit_mutation(&mut self, data_dir: &Path) -> Result<(), String> {
        if !self.is_degraded() {
            return Ok(());
        }

        // Degraded: re-probe on a throttle so recovery is automatic, without
        // hammering a wedged filesystem on every request.
        let now = now_ms();
        if now.saturating_sub(self.last_probe_ms) >= self.probe_interval.as_millis() as u64 {
            self.last_probe_ms = now;
            if Self::probe(data_dir, self.probe_bytes).is_ok() {
                self.recover();
                return Ok(());
            }
        }

        Err(self
            .reason()
            .unwrap_or("store is not accepting durable writes")
            .to_string())
    }

    /// Record that a persist attempt failed. Flips the gate to degraded.
    pub fn record_persist_failure(&mut self, command: &str, err: &str) {
        self.degrade(format!("persist failed after '{command}': {err}"));
    }

    /// Record that a persist attempt succeeded.
    pub fn record_persist_success(&mut self) {
        self.recover();
    }

    fn degrade(&mut self, reason: String) {
        // Start the throttle clock here. Without this `last_probe_ms` stays 0,
        // so the very next request re-probes immediately and a store that just
        // failed a real save gets hammered once per request — and, worse, a
        // probe small enough to squeeze onto a nearly-full disk would flip the
        // gate straight back to healthy.
        self.last_probe_ms = now_ms();

        let already = self.is_degraded();
        if !already {
            // Not a Warning. The store is diverging from disk.
            eprintln!(
                "kanban: 🔴 ENTERING DEGRADED MODE — {reason}. Mutations will be REFUSED; reads \
                 continue. Writes are not durable, and a restart would discard in-memory state. \
                 See HZ-6053."
            );
            self.health = Health::Degraded {
                reason,
                since_ms: now_ms(),
            };
        } else if let Health::Degraded { reason: r, .. } = &mut self.health {
            // Keep the latest cause without re-announcing on every request.
            *r = reason;
        }
    }

    fn recover(&mut self) {
        if self.is_degraded() {
            eprintln!("kanban: 🟢 recovered — the store is accepting durable writes again.");
        }
        self.health = Health::Healthy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_healthy_and_admits_mutations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut gate = HealthGate::new();
        assert!(!gate.is_degraded());
        assert!(gate.admit_mutation(dir.path()).is_ok());
    }

    #[test]
    fn probe_succeeds_on_a_writable_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(HealthGate::probe(dir.path(), 1024).is_ok());
    }

    /// The canary must not leave litter — otherwise it becomes the orphaned
    /// .tmp that CH-6055's recovery then quarantines on every restart.
    #[test]
    fn probe_leaves_no_litter() {
        let dir = tempfile::tempdir().expect("tempdir");
        HealthGate::probe(dir.path(), 1024).expect("probe");
        assert!(!dir.path().join("_probe.tmp").exists());
        let count = std::fs::read_dir(dir.path()).expect("read_dir").count();
        assert_eq!(count, 0, "probe left files behind");
    }

    #[test]
    fn probe_fails_on_an_unwritable_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("readonly");
        std::fs::create_dir(&target).expect("mkdir");
        let mut perms = std::fs::metadata(&target).expect("meta").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o500); // r-x: cannot create files
        }
        std::fs::set_permissions(&target, perms).expect("chmod");

        assert!(
            HealthGate::probe(&target, 1024).is_err(),
            "probe must fail on a read-only directory"
        );
    }

    /// The core contract: after a persist failure, mutations are REFUSED
    /// rather than accepted into a buffer a restart would discard.
    #[test]
    fn persist_failure_degrades_and_refuses_mutations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut gate = HealthGate::new();

        gate.record_persist_failure("create", "No space left on device (os error 28)");

        assert!(gate.is_degraded());
        let err = gate.admit_mutation(dir.path()).unwrap_err();
        assert!(
            err.contains("create"),
            "refusal should name the cause: {err}"
        );
        assert!(
            err.contains("No space left"),
            "refusal should carry the io error: {err}"
        );
    }

    #[test]
    fn degraded_reason_is_reported() {
        let mut gate = HealthGate::new();
        assert!(gate.reason().is_none());
        gate.record_persist_failure("move", "disk on fire");
        assert!(gate.reason().expect("reason").contains("disk on fire"));
    }

    /// Recovery must be automatic once the store accepts writes again —
    /// otherwise a transient full disk wedges the fleet until someone
    /// restarts the server by hand.
    #[test]
    fn recovers_automatically_once_the_store_is_writable() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Zero interval so the throttle does not gate the test.
        let mut gate = HealthGate::with_probe(1024, Duration::from_millis(0));
        gate.record_persist_failure("create", "No space left on device");
        assert!(gate.is_degraded());

        // The temp dir IS writable, so the next admission probes and recovers.
        assert!(gate.admit_mutation(dir.path()).is_ok());
        assert!(!gate.is_degraded());
    }

    /// While still unwritable, admission keeps refusing.
    #[test]
    fn stays_degraded_while_the_store_is_unwritable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("readonly");
        std::fs::create_dir(&target).expect("mkdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&target).expect("meta").permissions();
            perms.set_mode(0o500);
            std::fs::set_permissions(&target, perms).expect("chmod");
        }

        let mut gate = HealthGate::with_probe(1024, Duration::from_millis(0));
        gate.record_persist_failure("create", "No space left on device");
        assert!(gate.admit_mutation(&target).is_err());
        assert!(gate.is_degraded());
    }

    /// The throttle must actually throttle — a wedged store should not be
    /// probed once per request.
    #[test]
    fn probe_is_throttled_while_degraded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut gate = HealthGate::with_probe(1024, Duration::from_secs(3600));
        gate.record_persist_failure("create", "No space left on device");

        // Writable dir, but the long throttle means no probe runs yet, so the
        // gate must keep refusing rather than silently recovering.
        assert!(gate.admit_mutation(dir.path()).is_err());
        assert!(gate.is_degraded());
    }

    #[test]
    fn probe_now_degrades_on_startup_when_unwritable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("readonly");
        std::fs::create_dir(&target).expect("mkdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&target).expect("meta").permissions();
            perms.set_mode(0o500);
            std::fs::set_permissions(&target, perms).expect("chmod");
        }

        let mut gate = HealthGate::new();
        gate.probe_now(&target);
        assert!(
            gate.is_degraded(),
            "server must not start up claiming healthy on an unwritable store"
        );
    }

    #[test]
    fn probe_now_stays_healthy_on_a_writable_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut gate = HealthGate::new();
        gate.probe_now(dir.path());
        assert!(!gate.is_degraded());
    }

    #[test]
    fn persist_success_clears_degraded() {
        let mut gate = HealthGate::new();
        gate.record_persist_failure("create", "transient");
        assert!(gate.is_degraded());
        gate.record_persist_success();
        assert!(!gate.is_degraded());
    }
}
