//! HZ-6053 acceptance test (CH-6070) — the composite property, end to end.
//!
//! # What this proves
//!
//! > **Every write the server ACKNOWLEDGED is present after a restart.**
//!
//! That single invariant *is* the HZ-6053 incident. On 2026-07-18/19 a full disk
//! stopped the kanban server persisting. It kept serving correct state from
//! memory and kept returning **success** to agents, so `create` / `move` /
//! `pr merge` all looked like they landed — and a restart silently reverted the
//! board by two months. Nobody saw it until IDs from May started coming back.
//!
//! # Why a separate test rather than more unit tests
//!
//! Each mitigation is already unit-tested in isolation:
//!
//! - **CH-6056** — an unpersistable write is refused, not acked
//! - **CH-6055** — an interrupted save is quarantined; a `.tmp` newer than the
//!   live Parquet is flagged suspect instead of being silently superseded
//! - **CH-6054** — a frozen store alarms after a no-change streak
//!
//! All green, and the composite property holds *by construction*. But nothing
//! exercised the **sequence** (disk fills → mutations attempted → restart), and
//! "holds by construction" is not the bar for a hazard that took the fleet down
//! twice in two days. The tests below drive the whole arc and assert the
//! invariant directly.
//!
//! Fault injection is by **directory permissions**, not a real full disk —
//! portable, fast, and it exercises the same `ENOSPC`-shaped failure path
//! (`std::io::Error` out of the Parquet writer).

use nusy_kanban_server::handlers::dispatch;
use nusy_kanban_server::state::ServerState;

fn fresh_state(dir: &std::path::Path) -> ServerState {
    ServerState {
        store: nusy_kanban::persist::load_store(dir).expect("load store"),
        relations: nusy_kanban::persist::load_relations(dir).expect("load relations"),
        #[cfg(feature = "pr")]
        proposals: nusy_graph_review::ProposalStore::new(),
        #[cfg(feature = "pr")]
        comments: nusy_graph_review::CommentStore::new(),
        #[cfg(feature = "pr")]
        ci_results: nusy_graph_review::CiResultStore::new(),
        data_dir: dir.to_path_buf(),
        health: nusy_kanban_server::health::HealthGate::new(),
    }
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or(serde_json::Value::Null)
}

/// Attempt a create. Returns `Some(id)` only if the server ACKED it — i.e.
/// answered without an error code. That is the client's-eye view, and it is
/// exactly what the invariant is stated over.
fn try_create(state: &mut ServerState, title: &str) -> Option<String> {
    let payload = serde_json::to_vec(&serde_json::json!({
        "title": title,
        "item_type": "chore",
    }))
    .expect("encode");
    let resp = json(&dispatch("kanban.cmd.create", &payload, state));
    if resp.get("code").is_some() || resp.get("error").is_some() {
        None // refused or reported-failed — NOT an ack
    } else {
        resp["id"].as_str().map(str::to_string)
    }
}

#[cfg(unix)]
fn set_writable(dir: &std::path::Path, writable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(dir).expect("meta").permissions();
    perms.set_mode(if writable { 0o700 } else { 0o500 });
    std::fs::set_permissions(dir, perms).expect("chmod");
}

/// THE acceptance test. Drives the full HZ-6053 arc and asserts the invariant.
#[cfg(unix)]
#[test]
fn acked_writes_survive_a_restart_even_when_the_disk_fills() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // ── 1. Healthy baseline ──────────────────────────────────────────────
    let mut acked: Vec<String> = Vec::new();
    {
        let mut state = fresh_state(root);
        for i in 0..3 {
            let id =
                try_create(&mut state, &format!("baseline {i}")).expect("healthy server must ack");
            acked.push(id);
        }
    }
    let store_dir = nusy_kanban::persist::data_dir(root).expect("store dir");
    assert!(
        store_dir.join("items.parquet").exists(),
        "baseline should have persisted"
    );

    // ── 2. The disk fills ────────────────────────────────────────────────
    set_writable(&store_dir, false);

    // ── 3. Mutations under fault. Record only what the server ACKED. ─────
    let mut state = fresh_state(root);
    let mut acked_during_fault = Vec::new();
    for i in 0..5 {
        if let Some(id) = try_create(&mut state, &format!("during-fault {i}")) {
            acked_during_fault.push(id);
        }
    }

    // ── 4. Disk freed, server restarts (fresh load from disk) ────────────
    set_writable(&store_dir, true);
    let mut restarted = fresh_state(root);

    // ── 5. THE INVARIANT ─────────────────────────────────────────────────
    // Everything the server ever acked must still be there. Writes it REFUSED
    // may be absent — that is correct, and is the entire point of the gate.
    acked.extend(acked_during_fault.iter().cloned());
    let mut lost = Vec::new();
    for id in &acked {
        let payload = serde_json::to_vec(&serde_json::json!({ "id": id })).expect("encode");
        let resp = json(&dispatch("kanban.cmd.show", &payload, &mut restarted));
        if resp.get("error").is_some() || resp["id"].as_str() != Some(id.as_str()) {
            lost.push(id.clone());
        }
    }

    assert!(
        lost.is_empty(),
        "HZ-6053 INVARIANT VIOLATED — the server acked {} write(s) that a restart lost: {:?}. \
         This is the exact incident shape: success returned for work the board later forgot.",
        lost.len(),
        lost
    );
}

/// Negative control. The test above passes trivially if nothing is ever acked
/// during the fault, so prove the fault injection is real: under fault the
/// server must actually REFUSE, not quietly succeed.
///
/// Without this, a broken fault injection (e.g. permissions silently ignored)
/// would make the acceptance test green for the wrong reason.
#[cfg(unix)]
#[test]
fn the_fault_injection_actually_bites() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    {
        let mut state = fresh_state(root);
        try_create(&mut state, "seed").expect("healthy ack");
    }
    let store_dir = nusy_kanban::persist::data_dir(root).expect("store dir");
    set_writable(&store_dir, false);

    let mut state = fresh_state(root);
    let mut acks = 0;
    for i in 0..5 {
        if try_create(&mut state, &format!("under-fault {i}")).is_some() {
            acks += 1;
        }
    }
    set_writable(&store_dir, true);

    assert!(
        acks < 5,
        "fault injection did not bite — the server acked all 5 writes with an unwritable \
         store, so the acceptance test would pass for the wrong reason"
    );
    assert!(
        state.health.is_degraded(),
        "an unwritable store must degrade the health gate"
    );
}

/// A write the server REFUSED must not become readable.
///
/// This is the property the admission gate carries, and it is distinct from the
/// invariant above. With the persist-failure error conversion alone, a mutation
/// still lands in memory before the save is attempted — the client is correctly
/// told it failed, but the item is then visible to `show`/`list` until the next
/// restart quietly drops it. Two agents reading the same board would disagree
/// about whether the work exists, which is how the HZ-6053 divergence stayed
/// plausible for so long.
///
/// Admitting BEFORE the handler runs is what closes that: a refused mutation
/// never touches memory, so "failed" and "not there" always agree.
#[cfg(unix)]
#[test]
fn a_refused_write_never_becomes_readable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    {
        let mut state = fresh_state(root);
        try_create(&mut state, "seed").expect("healthy ack");
    }
    let store_dir = nusy_kanban::persist::data_dir(root).expect("store dir");
    set_writable(&store_dir, false);

    let mut state = fresh_state(root);
    let before = state.store.active_item_count();

    // Drive several mutations under fault; none should be acked after the gate trips.
    let mut refused = 0;
    for i in 0..4 {
        if try_create(&mut state, &format!("refused {i}")).is_none() {
            refused += 1;
        }
    }
    let after = state.store.active_item_count();
    set_writable(&store_dir, true);

    assert!(refused > 0, "expected at least one refusal under fault");
    // The first failure may legitimately land in memory (it is applied before the
    // save is attempted, then honestly reported as failed). Everything after the
    // gate trips must not — otherwise refused work accumulates as readable state.
    assert!(
        after - before <= 1,
        "refused writes leaked into readable state: {} item(s) appeared while the store was \
         unwritable and {refused} write(s) were refused. Only the first, pre-gate failure may \
         land in memory.",
        after - before
    );
}

/// Reads must keep working through the whole arc. A durability fix that takes
/// the board offline has traded one outage for another.
#[cfg(unix)]
#[test]
fn the_board_stays_readable_throughout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let seeded = {
        let mut state = fresh_state(root);
        try_create(&mut state, "readable through the incident").expect("ack")
    };
    let store_dir = nusy_kanban::persist::data_dir(root).expect("store dir");

    set_writable(&store_dir, false);
    let mut state = fresh_state(root);
    let payload = serde_json::to_vec(&serde_json::json!({ "id": &seeded })).expect("encode");
    let resp = json(&dispatch("kanban.cmd.show", &payload, &mut state));
    set_writable(&store_dir, true);

    assert_eq!(
        resp["id"].as_str(),
        Some(seeded.as_str()),
        "reads must survive the degraded window — a dark board is its own outage"
    );
}

/// The other half of the incident: an interrupted save left `items.parquet.tmp`
/// NEWER than the live Parquet. Recovery must quarantine it and flag the load
/// as suspect rather than silently serving the older state — that silent serve
/// is what made the revert invisible for two months.
#[test]
fn an_interrupted_save_is_never_silently_superseded() {
    use nusy_kanban::persistence::PersistenceEngine;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    {
        let mut state = fresh_state(root);
        try_create(&mut state, "live but stale").expect("ack");
    }
    let store_dir = nusy_kanban::persist::data_dir(root).expect("store dir");

    // A truncated interrupted write, strictly newer than the live Parquet.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(store_dir.join("items.parquet.tmp"), b"PAR1truncated").expect("write tmp");

    let rec = PersistenceEngine::recover_wal(root).expect("recover");

    assert!(rec.recovered, "an orphaned .tmp must trigger recovery");
    assert!(
        rec.is_suspect(),
        "a .tmp NEWER than the live Parquet means loading the live state is a rollback — \
         it must be flagged, not served silently"
    );
    let quarantine = rec.quarantine_dir.expect("quarantine dir");
    assert_eq!(
        std::fs::read_to_string(quarantine.join("items.parquet.tmp")).expect("read"),
        "PAR1truncated",
        "the interrupted write must be preserved verbatim — during HZ-6053 it was the only \
         copy of two months of state"
    );
    assert!(
        !store_dir.join("items.parquet.tmp").exists(),
        "the .tmp must be moved out of the store, not left for the next save to overwrite"
    );
}
