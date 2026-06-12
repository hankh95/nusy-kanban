//! EX-4337 / VY-4313 EX-vi — `nk regression compare` integration tests.
//!
//! Acceptance from the work item:
//! - Runs nightly without intervention
//! - Regression alerts work
//!
//! These tests synthesize two directories of `ScenariosPassResult` JSON
//! files (the runner output), then exercise `nk regression compare` end
//! to end via the `nusy_kanban::regression_cli` library API. Coverage:
//!
//! 1. Pairing — same battery on both sides → one pair, correct delta sign.
//! 2. Alert — V15→V16 regression beyond threshold surfaces an alert and
//!    `has_regressions()` flips true.
//! 3. Improvement — V15→V16 lift produces no alert and exit-clean signal.
//! 4. Threshold knob — same data with different thresholds yields different
//!    alert counts.
//! 5. Format — markdown vs JSON output written to disk; legacy JSON
//!    deserializes as ScenariosPassResult.
//! 6. Unpaired — battery only on one side is surfaced separately.
//!
//! The actual EX-iv runner (which requires GPU + vLLM) is NOT exercised
//! here; that's `scripts/nightly-regression.sh` and it's gated to DGX.

use std::fs;
use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use nusy_evaluator::grader::Grade;
use nusy_evaluator::provenance::PassResult;
use nusy_evaluator::scenarios::{GradeCounts, ScenariosPassResult};
use nusy_kanban::regression_cli::{RegressionCommands, compare_dirs, run};

fn pr(cq: &str, grade: Grade) -> PassResult {
    PassResult {
        cq_id: cq.to_string(),
        question: "q".into(),
        dimension: "word_meaning".into(),
        grade,
        response: "r".into(),
        matched_keywords: vec![],
        provenance: vec![],
        refusal_signal: false,
        persona_leak_signal: false,
    }
}

fn synth(
    being: &str,
    battery: &str,
    run_at: DateTime<Utc>,
    passes: usize,
    fails: usize,
) -> ScenariosPassResult {
    let mut per_cq = vec![];
    for i in 0..passes {
        per_cq.push(pr(&format!("CQ-PASS-{i}"), Grade::Pass));
    }
    for i in 0..fails {
        per_cq.push(pr(&format!("CQ-FAIL-{i}"), Grade::Fail));
    }
    let total = passes + fails;
    let rate = if total == 0 {
        0.0
    } else {
        passes as f64 / total as f64
    };
    ScenariosPassResult {
        being_label: being.to_string(),
        battery_label: battery.to_string(),
        run_at,
        per_cq,
        substantive_pass_rate: rate,
        substantive_eligible: total,
        substantive_passes: passes,
        liberal_pass_rate: rate,
        liberal_passes: passes,
        total_cqs: total,
        gap_list: (0..fails).map(|i| format!("CQ-FAIL-{i}")).collect(),
        grade_counts: GradeCounts {
            pass: passes,
            fail: fails,
            refuse: 0,
            persona_leak: 0,
            error: 0,
        },
        total_runtime_secs: Some(1.0),
        per_cq_runtime_secs: vec![0.1; total],
    }
}

fn write(dir: &Path, name: &str, run: &ScenariosPassResult) {
    fs::write(dir.join(name), serde_json::to_string_pretty(run).unwrap()).unwrap();
}

fn at(d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, d, 0, 0, 0).unwrap()
}

#[test]
fn end_to_end_compare_dirs_writes_json_report() {
    let tmp = tempfile::tempdir().unwrap();
    let baseline = tmp.path().join("v15");
    let candidate = tmp.path().join("v16");
    fs::create_dir_all(&baseline).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    write(
        &baseline,
        "alphabet.json",
        &synth("v15", "alphabet", at(1), 5, 5), // 50%
    );
    write(
        &candidate,
        "alphabet.json",
        &synth("v16", "alphabet", at(2), 8, 2), // 80% — +30pp improvement
    );
    let out = tmp.path().join("report.json");
    let outcome = compare_dirs(&baseline, &candidate, 5.0, Some(&out), None).expect("compare");
    assert!(out.exists(), "report file must be written");
    assert!(!outcome.has_regressions(), "improvement should not regress");
    let text = fs::read_to_string(&out).unwrap();
    let parsed: nusy_evaluator::regression::RegressionReport = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed.paired_count, 1);
    assert!(parsed.alerts.is_empty());
}

#[test]
fn regression_beyond_threshold_fires_alert_and_flips_exit_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let baseline = tmp.path().join("v15");
    let candidate = tmp.path().join("v16");
    fs::create_dir_all(&baseline).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    write(
        &baseline,
        "alphabet.json",
        &synth("v15", "alphabet", at(1), 8, 2), // 80%
    );
    write(
        &candidate,
        "alphabet.json",
        &synth("v16", "alphabet", at(2), 4, 6), // 40% — -40pp regression
    );
    let outcome = compare_dirs(&baseline, &candidate, 5.0, None, None).expect("compare");
    assert!(
        outcome.has_regressions(),
        "−40pp regression must trigger alert at 5pp threshold"
    );
    assert_eq!(outcome.report.alerts.len(), 1);
    assert_eq!(outcome.report.alerts[0].battery_label, "alphabet");
    assert!((outcome.report.alerts[0].delta_pp - (-40.0)).abs() < 1e-9);
}

#[test]
fn threshold_knob_changes_alert_count() {
    let tmp = tempfile::tempdir().unwrap();
    let baseline = tmp.path().join("v15");
    let candidate = tmp.path().join("v16");
    fs::create_dir_all(&baseline).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    write(
        &baseline,
        "easy.json",
        &synth("v15", "easy", at(1), 8, 2), // 80%
    );
    write(
        &baseline,
        "hard.json",
        &synth("v15", "hard", at(1), 5, 5), // 50%
    );
    write(
        &candidate,
        "easy.json",
        &synth("v15", "easy", at(2), 7, 3), // 70%  → -10pp
    );
    write(
        &candidate,
        "hard.json",
        &synth("v15", "hard", at(2), 4, 6), // 40%  → -10pp
    );

    // Loose threshold (15pp): no alerts.
    let loose = compare_dirs(&baseline, &candidate, 15.0, None, None).unwrap();
    assert_eq!(loose.report.alerts.len(), 0);

    // Tight threshold (5pp): both batteries alert.
    let tight = compare_dirs(&baseline, &candidate, 5.0, None, None).unwrap();
    assert_eq!(tight.report.alerts.len(), 2);
}

#[test]
fn markdown_format_renders_alert_table_and_creates_parent_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let baseline = tmp.path().join("v15");
    let candidate = tmp.path().join("v16");
    fs::create_dir_all(&baseline).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    write(
        &baseline,
        "alphabet.json",
        &synth("v15", "alphabet", at(1), 8, 2),
    );
    write(
        &candidate,
        "alphabet.json",
        &synth("v16", "alphabet", at(2), 2, 8), // -60pp
    );
    let nested = tmp.path().join("deep/path/report.md");
    let outcome = compare_dirs(&baseline, &candidate, 5.0, Some(&nested), None).unwrap();
    assert!(nested.exists());
    let md = fs::read_to_string(&nested).unwrap();
    assert!(md.contains("# Regression baseline report"));
    assert!(md.contains("## Alerts"));
    assert!(md.contains("alphabet"));
    assert!(md.contains("-60.0"));
    assert!(outcome.has_regressions());
}

#[test]
fn unpaired_batteries_surface_separately_without_alert() {
    let tmp = tempfile::tempdir().unwrap();
    let baseline = tmp.path().join("v15");
    let candidate = tmp.path().join("v16");
    fs::create_dir_all(&baseline).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    // Same battery on both sides at parity, plus one extra on each side.
    write(
        &baseline,
        "shared.json",
        &synth("v15", "shared", at(1), 5, 5),
    );
    write(
        &baseline,
        "v15_only.json",
        &synth("v15", "v15_only", at(1), 5, 5),
    );
    write(
        &candidate,
        "shared.json",
        &synth("v16", "shared", at(2), 5, 5),
    );
    write(
        &candidate,
        "v16_only.json",
        &synth("v16", "v16_only", at(2), 5, 5),
    );
    let outcome = compare_dirs(&baseline, &candidate, 5.0, None, None).unwrap();
    assert_eq!(outcome.report.paired_count, 1);
    assert_eq!(outcome.report.unpaired_baseline, vec!["v15_only"]);
    assert_eq!(outcome.report.unpaired_candidate, vec!["v16_only"]);
    assert!(
        !outcome.has_regressions(),
        "unpaired alone is not a regression"
    );
}

#[test]
fn run_subcommand_reports_regression_via_return_value() {
    // The dispatcher returns `Ok(true)` when regressions exist; main.rs
    // turns that into exit code 1.
    let tmp = tempfile::tempdir().unwrap();
    let baseline = tmp.path().join("v15");
    let candidate = tmp.path().join("v16");
    fs::create_dir_all(&baseline).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    write(&baseline, "b.json", &synth("v15", "b", at(1), 8, 2));
    write(&candidate, "b.json", &synth("v16", "b", at(2), 2, 8));
    let signal = run(RegressionCommands::Compare {
        baseline_dir: baseline,
        candidate_dir: candidate,
        threshold_pp: Some(5.0),
        out: None,
        format: None,
    })
    .expect("run");
    assert!(signal, "regression must signal non-zero-exit to main()");
}

#[test]
fn run_subcommand_clean_diff_returns_false() {
    let tmp = tempfile::tempdir().unwrap();
    let baseline = tmp.path().join("v15");
    let candidate = tmp.path().join("v16");
    fs::create_dir_all(&baseline).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    write(&baseline, "b.json", &synth("v15", "b", at(1), 5, 5));
    write(&candidate, "b.json", &synth("v16", "b", at(2), 7, 3));
    let signal = run(RegressionCommands::Compare {
        baseline_dir: baseline,
        candidate_dir: candidate,
        threshold_pp: None,
        out: None,
        format: None,
    })
    .expect("run");
    assert!(!signal, "improvement must return clean signal");
}

#[test]
fn ingestion_tolerates_legacy_json_without_runtime_fields() {
    // EX-4335-pre JSON. The runner historically didn't emit
    // total_runtime_secs / per_cq_runtime_secs; the regression compare
    // must keep ingesting legacy artifacts.
    let tmp = tempfile::tempdir().unwrap();
    let baseline = tmp.path().join("v15");
    let candidate = tmp.path().join("v16");
    fs::create_dir_all(&baseline).unwrap();
    fs::create_dir_all(&candidate).unwrap();

    let legacy_baseline = r#"{
        "being_label": "santiago-toddler-v15.4",
        "battery_label": "alphabet",
        "run_at": "2026-04-01T00:00:00Z",
        "per_cq": [],
        "substantive_pass_rate": 0.2,
        "substantive_eligible": 10,
        "substantive_passes": 2,
        "liberal_pass_rate": 0.2,
        "liberal_passes": 2,
        "total_cqs": 10,
        "gap_list": [],
        "grade_counts": {
            "pass": 2, "fail": 8, "refuse": 0, "persona_leak": 0, "error": 0
        }
    }"#;
    fs::write(baseline.join("legacy.json"), legacy_baseline).unwrap();
    write(
        &candidate,
        "alphabet.json",
        &synth("v16", "alphabet", at(2), 8, 2),
    );

    let outcome = compare_dirs(&baseline, &candidate, 5.0, None, None).expect("legacy ingest");
    assert_eq!(outcome.report.paired_count, 1);
    assert!((outcome.report.pairs[0].delta_pp - 60.0).abs() < 1e-9);
}
