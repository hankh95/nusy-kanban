//! `nk tutor` subcommands — review surface for V16 Customer's Plates.
//!
//! Plates are the ground-truth tutor records consumed by the EX-iv BDD
//! evaluator (`nusy-evaluator`). Auto-tutor (`nusy-tutor-auto`) generates
//! them; this surface gives reviewers a way to view, diff, and sign off on
//! the human-edited version before it becomes the eval target.
//!
//! Sidecar conventions used here:
//!
//! - `<stem>.expected.json` — current working plate (auto-generated then
//!   human-edited)
//! - `<stem>.auto.json` — immutable auto-tutor baseline; `tutor diff`
//!   compares against this
//! - `<stem>.review.json` — signoff sidecar written by `tutor approve`
//!
//! The signoff binds a reviewer to a specific plate hash. Re-edits invalidate
//! the signoff and `tutor status` reports the divergence.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use clap::Subcommand;
use nusy_tutor_record::{
    REVIEW_SCHEMA_VERSION, ReviewSignoff, ReviewStatus, TutorRecord, sha256_hex,
};

/// `nk tutor` subcommands.
#[derive(Subcommand, Clone)]
pub enum TutorCommands {
    /// View a plate's structure (and optionally open in $EDITOR).
    Review {
        /// Path to the plate (typically `*.expected.json`).
        plate: PathBuf,
        /// Open the plate in $EDITOR / $VISUAL. On save, validate before
        /// accepting; on validation failure, restore the original.
        #[arg(long)]
        edit: bool,
        /// If no `<stem>.auto.json` baseline exists yet, snapshot the
        /// current plate to that path before any edits. Idempotent.
        #[arg(long)]
        snapshot_baseline: bool,
    },
    /// Show the diff between a plate and its auto-baseline.
    Diff {
        /// Path to the (typically human-edited) plate.
        plate: PathBuf,
        /// Override the auto-baseline path (default: `<stem>.auto.json`).
        #[arg(long)]
        baseline: Option<PathBuf>,
    },
    /// Sign off on a plate. Writes `<stem>.review.json`.
    Approve {
        /// Path to the plate being approved.
        plate: PathBuf,
        /// Reviewer name (e.g., "Mini", "M5"). Required.
        #[arg(long)]
        reviewer: String,
        /// Optional reviewer comment.
        #[arg(long)]
        comment: Option<String>,
        /// Override the verdict (default: approved).
        #[arg(long, value_parser = ["approved", "rejected", "draft"], default_value = "approved")]
        status: String,
    },
    /// Report the review status of a plate (approved / draft / diverged).
    Status {
        /// Path to the plate.
        plate: PathBuf,
    },
}

/// Result of a status check.
#[derive(Debug, PartialEq)]
pub enum PlateReviewState {
    /// No `<stem>.review.json` sidecar found.
    NoSignoff,
    /// Sidecar present and the recorded plate hash matches the file.
    Signed(ReviewSignoff),
    /// Sidecar present but the plate has been edited since signoff.
    Diverged {
        signoff: ReviewSignoff,
        current_sha256: String,
    },
}

/// Compute the conventional `<stem>.auto.json` baseline path for a plate.
///
/// Strips a trailing `.expected.json` if present, otherwise just replaces
/// the extension. So `dame_wonder.expected.json` → `dame_wonder.auto.json`,
/// and `dame_wonder.json` → `dame_wonder.auto.json`.
pub fn baseline_path(plate: &Path) -> PathBuf {
    sibling_with_suffix(plate, "auto.json")
}

/// Compute the conventional `<stem>.review.json` signoff path.
pub fn signoff_path(plate: &Path) -> PathBuf {
    sibling_with_suffix(plate, "review.json")
}

fn sibling_with_suffix(plate: &Path, suffix: &str) -> PathBuf {
    let parent = plate.parent().unwrap_or_else(|| Path::new("."));
    let file_name = plate
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    // Strip a trailing `.expected.json` or `.auto.json` or `.review.json` so
    // that switching between sidecars is a stable transformation.
    let stem = file_name
        .strip_suffix(".expected.json")
        .or_else(|| file_name.strip_suffix(".auto.json"))
        .or_else(|| file_name.strip_suffix(".review.json"))
        .or_else(|| file_name.strip_suffix(".json"))
        .unwrap_or(file_name);
    parent.join(format!("{stem}.{suffix}"))
}

/// Load a plate from disk, parse it, and run the canonical validators.
fn load_plate(path: &Path) -> Result<(TutorRecord, Vec<u8>), String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let record: TutorRecord =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    record
        .validate_semantics()
        .map_err(|e| format!("invalid plate {}: {e}", path.display()))?;
    Ok((record, bytes))
}

/// Pretty-print a plate summary suitable for `nk tutor review` output.
pub fn render_summary(record: &TutorRecord, plate_path: &Path) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Plate: {}\n  schema_version: {}\n  document: {} (level={:?})\n  tutor: {} @ {}\n",
        plate_path.display(),
        record.schema_version,
        record.document.path,
        record.document.level,
        record.tutor.name,
        record.tutor.timestamp.to_rfc3339(),
    ));
    if let Some(gist) = &record.gist {
        out.push_str(&format!("  gist: {gist}\n"));
    }
    let l = &record.layers;
    out.push_str("\nLayers:\n");
    out.push_str(&format!(
        "  L1 literal:    {} chunks, {} triples (target={})\n",
        l.layer_1_literal.chunks.len(),
        l.layer_1_literal.triples.len(),
        fmt_target(l.layer_1_literal.target_count),
    ));
    out.push_str(&format!(
        "  L2 ontology:   {} entities (target={})\n",
        l.layer_2_ontology.entities.len(),
        fmt_target(l.layer_2_ontology.target_count),
    ));
    out.push_str(&format!(
        "  L3 curiosity:  {} CQs (target={})\n",
        l.layer_3_curiosity.cqs.len(),
        fmt_target(l.layer_3_curiosity.target_count),
    ));
    out.push_str(&format!(
        "  L4 cross-book: {} anchors (target={})\n",
        l.layer_4_cross_book.anchors.len(),
        fmt_target(l.layer_4_cross_book.target_count),
    ));
    out.push_str(&format!(
        "  L5 multimodal: {} illustrations (target={})\n",
        l.layer_5_multimodal.illustrations.len(),
        fmt_target(l.layer_5_multimodal.target_count),
    ));
    out
}

fn fmt_target(t: Option<usize>) -> String {
    match t {
        Some(n) => n.to_string(),
        None => "—".to_string(),
    }
}

/// `nk tutor review` — validate the plate, optionally snapshot a baseline,
/// and (with `--edit`) hand off to $EDITOR with rollback on validation
/// failure. Returns the rendered summary.
pub fn run_review(plate: &Path, edit: bool, snapshot_baseline: bool) -> Result<String, String> {
    let (record, bytes) = load_plate(plate)?;

    if snapshot_baseline {
        let baseline = baseline_path(plate);
        if !baseline.exists() {
            fs::write(&baseline, &bytes)
                .map_err(|e| format!("write baseline {}: {e}", baseline.display()))?;
        }
    }

    if edit {
        edit_in_place(plate)?;
        // Re-validate after edits.
        let _reloaded = load_plate(plate)?;
    }

    Ok(render_summary(&record, plate))
}

/// Open the plate in $EDITOR (falling back to $VISUAL, then `vi`). On exit
/// the file is reloaded and validated; if validation fails, the original
/// bytes are restored and an error is returned to the caller.
fn edit_in_place(plate: &Path) -> Result<(), String> {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    let original =
        fs::read(plate).map_err(|e| format!("read {} for edit: {e}", plate.display()))?;
    let status = Command::new(&editor)
        .arg(plate)
        .status()
        .map_err(|e| format!("launch {editor}: {e}"))?;
    if !status.success() {
        return Err(format!("{editor} exited with status {status}"));
    }
    if let Err(e) = load_plate(plate) {
        // Roll back to original bytes so a botched edit doesn't break the
        // plate file. Surface the original error.
        let _ = fs::write(plate, &original);
        return Err(format!("edit produced an invalid plate; reverted: {e}"));
    }
    Ok(())
}

/// `nk tutor diff` — return a unified-style diff between a plate and its
/// auto-baseline, both pretty-printed as JSON.
pub fn run_diff(plate: &Path, baseline_override: Option<&Path>) -> Result<String, String> {
    let (record, _) = load_plate(plate)?;
    let baseline = match baseline_override {
        Some(p) => p.to_path_buf(),
        None => baseline_path(plate),
    };
    if !baseline.exists() {
        return Err(format!(
            "no auto-baseline at {}. Generate the plate via auto-tutor or \
             run `nk tutor review --snapshot-baseline {}` first.",
            baseline.display(),
            plate.display(),
        ));
    }
    let (baseline_record, _) = load_plate(&baseline)?;

    let mut out = String::new();
    out.push_str(&format!(
        "--- {}\n+++ {}\n",
        baseline.display(),
        plate.display(),
    ));
    out.push_str(&render_layer_diff(&baseline_record, &record));
    Ok(out)
}

fn render_layer_diff(base: &TutorRecord, edit: &TutorRecord) -> String {
    let mut out = String::new();
    push_metadata_diff(&mut out, "gist", base.gist.as_deref(), edit.gist.as_deref());
    push_metadata_diff(
        &mut out,
        "notes",
        base.notes.as_deref(),
        edit.notes.as_deref(),
    );
    let bl = &base.layers;
    let el = &edit.layers;
    push_count_line(
        &mut out,
        "L1 chunks",
        bl.layer_1_literal.chunks.len(),
        el.layer_1_literal.chunks.len(),
    );
    push_count_line(
        &mut out,
        "L1 triples",
        bl.layer_1_literal.triples.len(),
        el.layer_1_literal.triples.len(),
    );
    push_count_line(
        &mut out,
        "L2 entities",
        bl.layer_2_ontology.entities.len(),
        el.layer_2_ontology.entities.len(),
    );
    push_count_line(
        &mut out,
        "L3 CQs",
        bl.layer_3_curiosity.cqs.len(),
        el.layer_3_curiosity.cqs.len(),
    );
    push_count_line(
        &mut out,
        "L4 anchors",
        bl.layer_4_cross_book.anchors.len(),
        el.layer_4_cross_book.anchors.len(),
    );
    push_count_line(
        &mut out,
        "L5 illustrations",
        bl.layer_5_multimodal.illustrations.len(),
        el.layer_5_multimodal.illustrations.len(),
    );

    // Triple-level adds/removes for L1 (the most-edited layer).
    let base_triples: std::collections::HashSet<_> = bl.layer_1_literal.triples.iter().collect();
    let edit_triples: std::collections::HashSet<_> = el.layer_1_literal.triples.iter().collect();
    let added: Vec<_> = edit_triples.difference(&base_triples).collect();
    let removed: Vec<_> = base_triples.difference(&edit_triples).collect();
    if !added.is_empty() || !removed.is_empty() {
        out.push_str("\nL1 triples:\n");
        for t in &removed {
            out.push_str(&format!(
                "- ({}, {}, {})\n",
                t.subject, t.predicate, t.object
            ));
        }
        for t in &added {
            out.push_str(&format!(
                "+ ({}, {}, {})\n",
                t.subject, t.predicate, t.object
            ));
        }
    }

    // CQ-level adds/removes for L3 (the second-most-edited layer).
    let base_cqs: std::collections::HashSet<_> = bl
        .layer_3_curiosity
        .cqs
        .iter()
        .map(|c| c.cq.as_str())
        .collect();
    let edit_cqs: std::collections::HashSet<_> = el
        .layer_3_curiosity
        .cqs
        .iter()
        .map(|c| c.cq.as_str())
        .collect();
    let cq_added: Vec<_> = edit_cqs.difference(&base_cqs).collect();
    let cq_removed: Vec<_> = base_cqs.difference(&edit_cqs).collect();
    if !cq_added.is_empty() || !cq_removed.is_empty() {
        out.push_str("\nL3 CQs:\n");
        for c in &cq_removed {
            out.push_str(&format!("- {c}\n"));
        }
        for c in &cq_added {
            out.push_str(&format!("+ {c}\n"));
        }
    }

    if out.lines().filter(|l| l.starts_with(['+', '-'])).count() == 0 {
        out.push_str("(no changes detected)\n");
    }
    out
}

fn push_count_line(out: &mut String, label: &str, base: usize, edit: usize) {
    if base != edit {
        let delta = edit as i64 - base as i64;
        out.push_str(&format!("{label:<18} {base} → {edit} ({delta:+})\n",));
    }
}

fn push_metadata_diff(out: &mut String, label: &str, base: Option<&str>, edit: Option<&str>) {
    if base != edit {
        out.push_str(&format!("\n{label}:\n"));
        if let Some(b) = base {
            out.push_str(&format!("- {b}\n"));
        }
        if let Some(e) = edit {
            out.push_str(&format!("+ {e}\n"));
        }
    }
}

/// `nk tutor approve` — write a `ReviewSignoff` sidecar bound to the plate's
/// current SHA-256 hash.
pub fn run_approve(
    plate: &Path,
    reviewer: &str,
    comment: Option<&str>,
    status: ReviewStatus,
) -> Result<(ReviewSignoff, PathBuf), String> {
    let (_record, bytes) = load_plate(plate)?;
    let plate_sha = sha256_hex(&bytes);

    let baseline = baseline_path(plate);
    let auto_baseline_sha256 = if baseline.exists() {
        let b = fs::read(&baseline)
            .map_err(|e| format!("read baseline {}: {e}", baseline.display()))?;
        Some(sha256_hex(&b))
    } else {
        None
    };

    let signoff = ReviewSignoff {
        schema_version: REVIEW_SCHEMA_VERSION.to_string(),
        plate_path: plate.to_string_lossy().into_owned(),
        plate_sha256: plate_sha,
        reviewer: reviewer.to_string(),
        approved_at: Utc::now(),
        status,
        comment: comment.map(str::to_string),
        auto_baseline_sha256,
    };

    let signoff_file = signoff_path(plate);
    let json =
        serde_json::to_string_pretty(&signoff).map_err(|e| format!("serialize signoff: {e}"))?;
    let mut f = fs::File::create(&signoff_file)
        .map_err(|e| format!("create {}: {e}", signoff_file.display()))?;
    f.write_all(json.as_bytes())
        .map_err(|e| format!("write {}: {e}", signoff_file.display()))?;
    f.write_all(b"\n").ok();

    Ok((signoff, signoff_file))
}

/// `nk tutor status` — load the signoff sidecar (if any) and check whether
/// the plate's current contents still match the approved hash.
pub fn run_status(plate: &Path) -> Result<PlateReviewState, String> {
    let bytes = fs::read(plate).map_err(|e| format!("read {}: {e}", plate.display()))?;
    let current_sha = sha256_hex(&bytes);

    let signoff_file = signoff_path(plate);
    if !signoff_file.exists() {
        return Ok(PlateReviewState::NoSignoff);
    }
    let signoff_bytes =
        fs::read(&signoff_file).map_err(|e| format!("read {}: {e}", signoff_file.display()))?;
    let signoff: ReviewSignoff = serde_json::from_slice(&signoff_bytes)
        .map_err(|e| format!("parse {}: {e}", signoff_file.display()))?;
    if signoff.plate_sha256 == current_sha {
        Ok(PlateReviewState::Signed(signoff))
    } else {
        Ok(PlateReviewState::Diverged {
            signoff,
            current_sha256: current_sha,
        })
    }
}

/// Format a `PlateReviewState` for `nk tutor status` output.
pub fn render_status(state: &PlateReviewState) -> String {
    match state {
        PlateReviewState::NoSignoff => "draft (no signoff sidecar found)".to_string(),
        PlateReviewState::Signed(s) => format!(
            "{:?} by {} at {} (sha256={}…)",
            s.status,
            s.reviewer,
            s.approved_at.to_rfc3339(),
            &s.plate_sha256[..16.min(s.plate_sha256.len())],
        ),
        PlateReviewState::Diverged {
            signoff,
            current_sha256,
        } => format!(
            "DIVERGED — last signoff: {:?} by {} at {} (sha256={}…); current sha256={}…",
            signoff.status,
            signoff.reviewer,
            signoff.approved_at.to_rfc3339(),
            &signoff.plate_sha256[..16.min(signoff.plate_sha256.len())],
            &current_sha256[..16.min(current_sha256.len())],
        ),
    }
}

/// Top-level dispatch from main.rs.
pub fn run(cmd: TutorCommands) -> Result<(), String> {
    match cmd {
        TutorCommands::Review {
            plate,
            edit,
            snapshot_baseline,
        } => {
            let summary = run_review(&plate, edit, snapshot_baseline)?;
            print!("{summary}");
            // Append signoff status so reviewers see it without a second call.
            let state = run_status(&plate)?;
            println!("\nReview status: {}", render_status(&state));
            Ok(())
        }
        TutorCommands::Diff { plate, baseline } => {
            let diff = run_diff(&plate, baseline.as_deref())?;
            print!("{diff}");
            Ok(())
        }
        TutorCommands::Approve {
            plate,
            reviewer,
            comment,
            status,
        } => {
            let parsed_status = parse_status(&status)?;
            let (signoff, path) =
                run_approve(&plate, &reviewer, comment.as_deref(), parsed_status)?;
            println!(
                "Wrote signoff to {} (status={:?}, sha256={}…).",
                path.display(),
                signoff.status,
                &signoff.plate_sha256[..16.min(signoff.plate_sha256.len())],
            );
            Ok(())
        }
        TutorCommands::Status { plate } => {
            let state = run_status(&plate)?;
            println!("{}", render_status(&state));
            Ok(())
        }
    }
}

fn parse_status(s: &str) -> Result<ReviewStatus, String> {
    match s {
        "approved" => Ok(ReviewStatus::Approved),
        "rejected" => Ok(ReviewStatus::Rejected),
        "draft" => Ok(ReviewStatus::Draft),
        other => Err(format!("invalid status: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_path_strips_expected_suffix() {
        assert_eq!(
            baseline_path(Path::new("plates/dame_wonder.expected.json")),
            PathBuf::from("plates/dame_wonder.auto.json"),
        );
    }

    #[test]
    fn baseline_path_handles_plain_json() {
        assert_eq!(
            baseline_path(Path::new("dame_wonder.json")),
            PathBuf::from("dame_wonder.auto.json"),
        );
    }

    #[test]
    fn signoff_path_strips_expected_suffix() {
        assert_eq!(
            signoff_path(Path::new("plates/x.expected.json")),
            PathBuf::from("plates/x.review.json"),
        );
    }

    #[test]
    fn signoff_path_strips_auto_suffix() {
        // Re-keying off an auto-baseline should still yield the canonical
        // `<stem>.review.json` location.
        assert_eq!(
            signoff_path(Path::new("plates/x.auto.json")),
            PathBuf::from("plates/x.review.json"),
        );
    }

    #[test]
    fn parse_status_recognizes_three_verdicts() {
        assert_eq!(parse_status("approved").unwrap(), ReviewStatus::Approved);
        assert_eq!(parse_status("rejected").unwrap(), ReviewStatus::Rejected);
        assert_eq!(parse_status("draft").unwrap(), ReviewStatus::Draft);
        assert!(parse_status("yikes").is_err());
    }
}
