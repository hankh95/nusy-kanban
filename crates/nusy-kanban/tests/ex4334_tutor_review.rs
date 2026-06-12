//! EX-4334 / VY-4313 EX-iii — `nk tutor` review surface.
//!
//! Acceptance from the work item: "CLI in place; one round-trip review
//! observable on Dame Wonder." These tests exercise the full review
//! lifecycle on the canonical Dame Wonder fixture from
//! `nusy-tutor-record/examples/dame_wonder.json`:
//!
//! 1. Snapshot an auto-baseline next to the plate.
//! 2. Edit a triple in the working plate and call `tutor diff` — the diff
//!    must capture the structural change.
//! 3. Approve the edited plate, writing a `*.review.json` sidecar bound to
//!    the file's SHA-256.
//! 4. Re-edit the plate after approval — `tutor status` must report the
//!    signoff as DIVERGED.
//! 5. Confirm `tutor review --snapshot-baseline` is idempotent (a second
//!    call doesn't clobber the original baseline).
//!
//! This is end-to-end: the test calls the public functions exposed by
//! `nusy_kanban::tutor_cli` (the CLI dispatch is a thin wrapper around
//! these), exercising the real Dame Wonder plate the EX-iv evaluator will
//! grade against.

use std::fs;
use std::path::PathBuf;

use nusy_kanban::tutor_cli::{
    PlateReviewState, baseline_path, render_status, render_summary, run_approve, run_diff,
    run_review, run_status, signoff_path,
};
use nusy_tutor_record::{ReviewStatus, TutorRecord};

const DAME_WONDER_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../nusy-tutor-record/examples/dame_wonder.json",
);

fn copy_fixture_to(dir: &std::path::Path, file_name: &str) -> PathBuf {
    let dst = dir.join(file_name);
    fs::copy(DAME_WONDER_FIXTURE, &dst).expect("dame_wonder fixture must be readable");
    dst
}

#[test]
fn dame_wonder_round_trip_review_to_approve_to_status() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plate = copy_fixture_to(tmp.path(), "dame_wonder.expected.json");

    // Step 1 — `tutor review --snapshot-baseline` materializes the baseline
    // and prints a non-empty summary.
    let summary = run_review(&plate, false, true).expect("review snapshot");
    assert!(summary.contains("Plate:"));
    assert!(summary.contains("L1 literal"));
    assert!(
        baseline_path(&plate).exists(),
        "baseline must be snapshotted"
    );

    // Step 2 — pre-approval status is "no signoff".
    let s0 = run_status(&plate).expect("status pre-approval");
    assert_eq!(s0, PlateReviewState::NoSignoff);

    // Step 3 — approve. Sidecar is written and `tutor status` reports
    // Signed.
    let (signoff, sidecar) = run_approve(
        &plate,
        "Mini",
        Some("Round-trip test"),
        ReviewStatus::Approved,
    )
    .expect("approve");
    assert!(sidecar.exists());
    assert_eq!(sidecar, signoff_path(&plate));
    assert_eq!(signoff.reviewer, "Mini");
    assert_eq!(signoff.status, ReviewStatus::Approved);
    assert_eq!(signoff.plate_sha256.len(), 64);
    assert!(
        signoff.auto_baseline_sha256.is_some(),
        "baseline existed at signoff so its hash must be recorded"
    );

    let s1 = run_status(&plate).expect("status after approval");
    match s1 {
        PlateReviewState::Signed(s) => {
            assert_eq!(s.reviewer, "Mini");
            assert_eq!(s.status, ReviewStatus::Approved);
        }
        other => panic!("expected Signed, got {other:?}"),
    }

    // Step 4 — edit the plate (touch the gist) and check status reports
    // DIVERGED. We mutate via TutorRecord to keep the file schema-valid.
    let mut record: TutorRecord =
        serde_json::from_slice(&fs::read(&plate).expect("read plate")).expect("parse plate");
    record.gist = Some("Edited gist that wasn't approved.".to_string());
    fs::write(&plate, serde_json::to_vec_pretty(&record).unwrap()).expect("write edited plate");

    let s2 = run_status(&plate).expect("status after edit");
    match s2 {
        PlateReviewState::Diverged {
            signoff,
            current_sha256,
        } => {
            assert_eq!(signoff.reviewer, "Mini");
            assert_ne!(signoff.plate_sha256, current_sha256);
            let rendered = render_status(&PlateReviewState::Diverged {
                signoff: signoff.clone(),
                current_sha256: current_sha256.clone(),
            });
            assert!(
                rendered.contains("DIVERGED"),
                "rendered status must call out divergence: {rendered}"
            );
        }
        other => panic!("expected Diverged after edit, got {other:?}"),
    }
}

#[test]
fn diff_against_baseline_captures_layer_1_triple_changes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plate = copy_fixture_to(tmp.path(), "dame_wonder.expected.json");

    // Snapshot baseline first.
    run_review(&plate, false, true).unwrap();

    // Mutate L1 triples — drop one and add one. Diff must catch both.
    let mut record: TutorRecord = serde_json::from_slice(&fs::read(&plate).unwrap()).unwrap();
    let removed = record
        .layers
        .layer_1_literal
        .triples
        .pop()
        .expect("at least one L1 triple in fixture");
    record
        .layers
        .layer_1_literal
        .triples
        .push(nusy_tutor_record::Triple {
            subject: "ex4334:test_subject".into(),
            predicate: "ex4334:test_predicate".into(),
            object: "ex4334:test_object".into(),
            provenance: None,
            notes: None,
        });
    fs::write(&plate, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    let diff = run_diff(&plate, None).expect("diff");
    assert!(
        diff.contains("--- "),
        "diff header must include baseline path"
    );
    assert!(
        diff.contains("ex4334:test_subject"),
        "added triple must show"
    );
    assert!(
        diff.contains(&removed.subject),
        "removed triple's subject must show; subj={}",
        removed.subject
    );
    assert!(
        diff.contains("L1 triples:"),
        "L1 triple section must be present"
    );
}

#[test]
fn diff_captures_gist_and_notes_changes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plate = copy_fixture_to(tmp.path(), "dame_wonder.expected.json");
    run_review(&plate, false, true).unwrap();

    let mut record: TutorRecord = serde_json::from_slice(&fs::read(&plate).unwrap()).unwrap();
    record.gist = Some("Reviewer-edited gist for clarity.".into());
    record.notes = Some("Added reviewer notes.".into());
    fs::write(&plate, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    let diff = run_diff(&plate, None).expect("diff");
    assert!(
        diff.contains("gist:"),
        "metadata diff must show gist: {diff}"
    );
    assert!(
        diff.contains("Reviewer-edited gist for clarity."),
        "edited gist must appear in diff: {diff}"
    );
    assert!(
        diff.contains("notes:"),
        "metadata diff must show notes: {diff}"
    );
    assert!(
        !diff.contains("(no changes detected)"),
        "metadata edits must not be flagged as no-change"
    );
}

#[test]
fn diff_with_missing_baseline_returns_actionable_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plate = copy_fixture_to(tmp.path(), "dame_wonder.expected.json");

    // Don't snapshot — call diff directly. Must error helpfully.
    let err = run_diff(&plate, None).expect_err("diff must fail without baseline");
    assert!(
        err.contains("snapshot-baseline") || err.contains("auto-baseline"),
        "error must point reviewer at the fix: {err}"
    );
}

#[test]
fn snapshot_baseline_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plate = copy_fixture_to(tmp.path(), "dame_wonder.expected.json");

    run_review(&plate, false, true).unwrap();
    let baseline_bytes_first = fs::read(baseline_path(&plate)).unwrap();

    // Mutate the working plate so that *if* snapshot were not idempotent,
    // a second call would overwrite the baseline with the new contents.
    let mut record: TutorRecord = serde_json::from_slice(&fs::read(&plate).unwrap()).unwrap();
    record.notes = Some("post-baseline edit".into());
    fs::write(&plate, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    run_review(&plate, false, true).unwrap();
    let baseline_bytes_second = fs::read(baseline_path(&plate)).unwrap();
    assert_eq!(
        baseline_bytes_first, baseline_bytes_second,
        "second snapshot must not overwrite the original baseline"
    );
}

#[test]
fn signoff_records_baseline_hash_only_when_baseline_exists() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plate = copy_fixture_to(tmp.path(), "dame_wonder.expected.json");

    // Approve without snapshotting first — baseline absent.
    let (signoff, _) = run_approve(&plate, "Mini", None, ReviewStatus::Approved).unwrap();
    assert!(signoff.auto_baseline_sha256.is_none());

    // Snapshot baseline, approve again — hash now recorded.
    run_review(&plate, false, true).unwrap();
    let (signoff2, _) = run_approve(&plate, "Mini", None, ReviewStatus::Approved).unwrap();
    assert!(signoff2.auto_baseline_sha256.is_some());
    assert_eq!(signoff2.auto_baseline_sha256.unwrap().len(), 64);
}

#[test]
fn render_summary_lists_all_five_layers() {
    let bytes = fs::read(DAME_WONDER_FIXTURE).expect("read fixture");
    let record: TutorRecord = serde_json::from_slice(&bytes).expect("parse");
    let summary = render_summary(&record, std::path::Path::new("dame.json"));
    for layer_label in [
        "L1 literal",
        "L2 ontology",
        "L3 curiosity",
        "L4 cross-book",
        "L5 multimodal",
    ] {
        assert!(
            summary.contains(layer_label),
            "summary missing {layer_label}: {summary}"
        );
    }
}
