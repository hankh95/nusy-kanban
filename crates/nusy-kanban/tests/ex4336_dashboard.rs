//! EX-4336 / VY-4313 EX-v — `nk dashboard render` integration tests.
//!
//! Acceptance from the work item:
//! - Dashboard reflects live test runs
//! - Trend over multiple runs visible
//!
//! These tests synthesize a directory of `ScenariosPassResult` JSON files
//! exactly the way EX-iv (`nusy_evaluator::scenarios::test_scenarios`)
//! emits them, then exercise the public `nusy_evaluator::dashboard` API +
//! the `nk dashboard render` shim. They cover:
//!
//! 1. End-to-end load → build → render of HTML, with assertions on every
//!    section (headline, latest, per-being trend, per-CQ failures).
//! 2. Multi-run trend — three runs of the same being on the same battery
//!    must render in chronological order and surface deltas.
//! 3. Per-CQ heatmap — a CQ that fails every time must rank above CQs that
//!    pass.
//! 4. The legacy JSON format from before EX-4335 (no `total_runtime_secs`,
//!    no `per_cq_runtime_secs`) must still ingest cleanly.

use std::fs;
use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use nusy_evaluator::dashboard::{
    DashboardModel, build_model, load_results_from_dir, render_html, render_markdown,
};
use nusy_evaluator::grader::Grade;
use nusy_evaluator::provenance::PassResult;
use nusy_evaluator::scenarios::{GradeCounts, ScenariosPassResult};

fn pass_result(cq_id: &str, question: &str, dimension: &str, grade: Grade) -> PassResult {
    PassResult {
        cq_id: cq_id.to_string(),
        question: question.to_string(),
        dimension: dimension.to_string(),
        grade,
        response: format!("response for {cq_id}"),
        matched_keywords: vec![],
        provenance: vec![],
        refusal_signal: matches!(grade, Grade::Refuse),
        persona_leak_signal: matches!(grade, Grade::PersonaLeak),
    }
}

fn synthesize_run(
    being: &str,
    battery: &str,
    run_at: DateTime<Utc>,
    per_cq: Vec<PassResult>,
) -> ScenariosPassResult {
    let total_cqs = per_cq.len();
    let mut counts = GradeCounts::default();
    for r in &per_cq {
        match r.grade {
            Grade::Pass => counts.pass += 1,
            Grade::Fail => counts.fail += 1,
            Grade::Refuse => counts.refuse += 1,
            Grade::PersonaLeak => counts.persona_leak += 1,
            Grade::Error => counts.error += 1,
        }
    }
    let substantive_eligible = total_cqs;
    let substantive_passes = counts.pass;
    let substantive_pass_rate = if substantive_eligible == 0 {
        0.0
    } else {
        substantive_passes as f64 / substantive_eligible as f64
    };
    let liberal_passes = counts.pass + counts.refuse;
    let liberal_pass_rate = if total_cqs == 0 {
        0.0
    } else {
        liberal_passes as f64 / total_cqs as f64
    };
    let gap_list = per_cq
        .iter()
        .filter(|r| matches!(r.grade, Grade::Fail | Grade::PersonaLeak | Grade::Error))
        .map(|r| r.cq_id.clone())
        .collect();
    ScenariosPassResult {
        being_label: being.to_string(),
        battery_label: battery.to_string(),
        run_at,
        per_cq,
        substantive_pass_rate,
        substantive_eligible,
        substantive_passes,
        liberal_pass_rate,
        liberal_passes,
        total_cqs,
        gap_list,
        grade_counts: counts,
        total_runtime_secs: Some(12.5),
        per_cq_runtime_secs: vec![0.5; total_cqs],
    }
}

fn write_run(dir: &Path, name: &str, run: &ScenariosPassResult) {
    let json = serde_json::to_string_pretty(run).unwrap();
    fs::write(dir.join(name), json).unwrap();
}

#[test]
fn end_to_end_load_build_render_html_covers_all_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 5, 5, 12, 0, 0).unwrap();
    let v15 = synthesize_run(
        "santiago-toddler-v15",
        "00_alphabet_dame_wonder",
        when,
        vec![
            pass_result("CQ-001", "What is an archer?", "word_meaning", Grade::Pass),
            pass_result("CQ-002", "What is a bow?", "word_meaning", Grade::Fail),
            pass_result("CQ-003", "Why does it work?", "causal", Grade::Refuse),
        ],
    );
    let v16 = synthesize_run(
        "santiago-toddler-v16",
        "00_alphabet_dame_wonder",
        when,
        vec![
            pass_result("CQ-001", "What is an archer?", "word_meaning", Grade::Pass),
            pass_result("CQ-002", "What is a bow?", "word_meaning", Grade::Pass),
            pass_result("CQ-003", "Why does it work?", "causal", Grade::Refuse),
        ],
    );
    write_run(tmp.path(), "v15_dame.json", &v15);
    write_run(tmp.path(), "v16_dame.json", &v16);

    let results = load_results_from_dir(tmp.path()).expect("load");
    assert_eq!(results.len(), 2);
    let model = build_model(&results, &tmp.path().display().to_string());
    let html = render_html(&model);

    // Headline — both beings counted.
    assert!(html.contains("Beings tested"));
    assert!(html.contains("santiago-toddler-v15"));
    assert!(html.contains("santiago-toddler-v16"));

    // Latest table — both rows.
    assert!(html.contains("00_alphabet_dame_wonder"));

    // Pass-rate styling: v16 (66.7%) → warn, v15 (33.3%) → bad.
    assert!(html.contains("66.7%"));
    assert!(html.contains("33.3%"));

    // Per-CQ section — CQ-002 fails for v15 only; render must list it.
    assert!(html.contains("CQ-002"));
    assert!(html.contains("Per-CQ failure breakdown"));

    // Trend section appears even with one run per being.
    assert!(html.contains("Per-being trend"));
}

#[test]
fn trend_over_three_runs_is_chronological_with_visible_progression() {
    let tmp = tempfile::tempdir().unwrap();
    // Three runs of v16 over time with progressively better pass rates.
    let make = |day: u32, passes: usize, fails: usize| -> ScenariosPassResult {
        let mut cqs = vec![];
        for i in 0..passes {
            cqs.push(pass_result(
                &format!("CQ-PASS-{i}"),
                "q",
                "word_meaning",
                Grade::Pass,
            ));
        }
        for i in 0..fails {
            cqs.push(pass_result(
                &format!("CQ-FAIL-{i}"),
                "q",
                "word_meaning",
                Grade::Fail,
            ));
        }
        synthesize_run(
            "santiago-toddler-v16",
            "alphabet",
            Utc.with_ymd_and_hms(2026, 5, day, 0, 0, 0).unwrap(),
            cqs,
        )
    };
    write_run(tmp.path(), "run_d1.json", &make(1, 1, 4)); // 20%
    write_run(tmp.path(), "run_d3.json", &make(3, 3, 2)); // 60%
    write_run(tmp.path(), "run_d5.json", &make(5, 4, 1)); // 80%

    let results = load_results_from_dir(tmp.path()).unwrap();
    let model = build_model(&results, "trend-test");

    // One being with three trend points, sorted ascending.
    assert_eq!(model.trends.len(), 1);
    let pts = &model.trends[0].points;
    assert_eq!(pts.len(), 3);
    assert!(
        (pts[0].substantive_pass_rate - 0.20).abs() < 1e-9,
        "first run: 20%; got {}",
        pts[0].substantive_pass_rate
    );
    assert!((pts[1].substantive_pass_rate - 0.60).abs() < 1e-9);
    assert!((pts[2].substantive_pass_rate - 0.80).abs() < 1e-9);

    // Markdown rendering must show the deltas.
    let md = render_markdown(&model);
    assert!(md.contains("santiago-toddler-v16"));
    assert!(
        md.contains("+40.0pp"),
        "60% - 20% = 40pp delta missing: {md}"
    );
    assert!(
        md.contains("+20.0pp"),
        "80% - 60% = 20pp delta missing: {md}"
    );
}

#[test]
fn per_cq_failures_rank_persistent_failures_above_passes() {
    let tmp = tempfile::tempdir().unwrap();
    // Three runs of the same battery: CQ-PERSISTENT fails every time;
    // CQ-RELIABLE passes every time. Only CQ-PERSISTENT should top the
    // failure leaderboard.
    let when = |day: u32| Utc.with_ymd_and_hms(2026, 5, day, 0, 0, 0).unwrap();
    for day in 1..=3 {
        let run = synthesize_run(
            "santiago-toddler-v16",
            "alphabet",
            when(day),
            vec![
                pass_result("CQ-PERSISTENT", "Why?", "causal", Grade::Fail),
                pass_result("CQ-RELIABLE", "What?", "word_meaning", Grade::Pass),
            ],
        );
        write_run(tmp.path(), &format!("d{day}.json"), &run);
    }
    let results = load_results_from_dir(tmp.path()).unwrap();
    let model = build_model(&results, "rank-test");
    assert_eq!(model.cq_failures.len(), 2);
    assert_eq!(model.cq_failures[0].cq_id, "CQ-PERSISTENT");
    assert!(
        (model.cq_failures[0].substantive_failure_rate - 1.0).abs() < 1e-9,
        "persistent failure rate must be 1.0; got {}",
        model.cq_failures[0].substantive_failure_rate,
    );
    assert!(
        (model.cq_failures[1].substantive_failure_rate - 0.0).abs() < 1e-9,
        "reliable failure rate must be 0.0; got {}",
        model.cq_failures[1].substantive_failure_rate,
    );
}

#[test]
fn legacy_json_without_runtime_fields_ingests_cleanly() {
    // Pre-EX-4335 ScenariosPassResult JSON had no total_runtime_secs and
    // no per_cq_runtime_secs. The dashboard MUST keep ingesting them so
    // historical run archives remain visible.
    let tmp = tempfile::tempdir().unwrap();
    let legacy = r#"{
        "being_label": "santiago-toddler-v15",
        "battery_label": "alphabet",
        "run_at": "2026-04-01T00:00:00Z",
        "per_cq": [
            {
                "cq_id": "CQ-001",
                "question": "What is an archer?",
                "dimension": "word_meaning",
                "grade": "pass",
                "response": "An archer uses a bow.",
                "matched_keywords": ["archer", "bow"],
                "provenance": [],
                "refusal_signal": false,
                "persona_leak_signal": false
            }
        ],
        "substantive_pass_rate": 1.0,
        "substantive_eligible": 1,
        "substantive_passes": 1,
        "liberal_pass_rate": 1.0,
        "liberal_passes": 1,
        "total_cqs": 1,
        "gap_list": [],
        "grade_counts": {
            "pass": 1, "fail": 0, "refuse": 0, "persona_leak": 0, "error": 0
        }
    }"#;
    fs::write(tmp.path().join("legacy.json"), legacy).unwrap();
    let results = load_results_from_dir(tmp.path()).expect("legacy must parse");
    assert_eq!(results.len(), 1);
    assert!(results[0].total_runtime_secs.is_none());
    assert!(results[0].per_cq_runtime_secs.is_empty());
    let model = build_model(&results, "legacy-test");
    let html = render_html(&model);
    // Runtime column should fall back to "—" markup, not crash.
    assert!(html.contains("santiago-toddler-v15"));
}

#[test]
fn malformed_json_in_results_dir_is_surfaced_as_parse_error() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("bad.json"), "{ not valid json }").unwrap();
    let err = load_results_from_dir(tmp.path()).expect_err("must fail");
    let msg = err.to_string();
    assert!(msg.contains("parse"));
    assert!(msg.contains("bad.json"));
}

#[test]
fn non_json_files_in_results_dir_are_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("notes.md"), "# scratch notes").unwrap();
    fs::write(tmp.path().join("data.txt"), "not json").unwrap();
    let when = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
    write_run(
        tmp.path(),
        "real.json",
        &synthesize_run(
            "santiago-toddler-v16",
            "alphabet",
            when,
            vec![pass_result("CQ-1", "q", "d", Grade::Pass)],
        ),
    );
    let results = load_results_from_dir(tmp.path()).unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn nk_dashboard_render_writes_html_to_out_path() {
    use nusy_kanban::dashboard_cli::{DashboardCommands, run};
    let tmp = tempfile::tempdir().unwrap();
    let results_dir = tmp.path().join("runs");
    fs::create_dir(&results_dir).unwrap();
    let when = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
    write_run(
        &results_dir,
        "v16.json",
        &synthesize_run(
            "santiago-toddler-v16",
            "alphabet",
            when,
            vec![pass_result("CQ-1", "What?", "word_meaning", Grade::Pass)],
        ),
    );
    let out = tmp.path().join("metrics.html");
    run(DashboardCommands::Render {
        results_dir: results_dir.clone(),
        out: Some(out.clone()),
        format: "html".to_string(),
    })
    .expect("render");
    let html = fs::read_to_string(&out).unwrap();
    assert!(html.contains("<title>VY-4313 metrics dashboard</title>"));
    assert!(html.contains("santiago-toddler-v16"));
}

#[test]
fn nk_dashboard_render_creates_parent_directories() {
    use nusy_kanban::dashboard_cli::{DashboardCommands, run};
    let tmp = tempfile::tempdir().unwrap();
    let results_dir = tmp.path().join("runs");
    fs::create_dir(&results_dir).unwrap();
    let when = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
    write_run(
        &results_dir,
        "v16.json",
        &synthesize_run(
            "santiago-toddler-v16",
            "b",
            when,
            vec![pass_result("CQ-1", "q", "d", Grade::Pass)],
        ),
    );
    let out = tmp.path().join("nested/dirs/metrics.md");
    run(DashboardCommands::Render {
        results_dir,
        out: Some(out.clone()),
        format: "markdown".to_string(),
    })
    .expect("render");
    assert!(out.exists(), "parent dirs must be created");
    let md = fs::read_to_string(&out).unwrap();
    assert!(md.starts_with("# VY-4313 metrics dashboard"));
}

#[test]
fn dashboard_model_is_serializable_for_downstream_consumers() {
    // EX-4337 (nightly regression runner) consumes this model. Lock the
    // serialization contract so it can't drift accidentally.
    let when = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
    let run = synthesize_run(
        "santiago-toddler-v16",
        "alphabet",
        when,
        vec![pass_result("CQ-1", "q", "d", Grade::Pass)],
    );
    let model = build_model(std::slice::from_ref(&run), "test");
    let json = serde_json::to_string(&model).expect("DashboardModel: Serialize");
    let _back: DashboardModel = serde_json::from_str(&json).expect("DashboardModel: Deserialize");
}
