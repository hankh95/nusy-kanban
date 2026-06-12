//! EX-4335 (EX-iv) — runtime-budget assertion tests for the BDD test runner.
//!
//! The chore body's acceptance criterion is:
//!     "Runs in <5 min per book against full plate"
//!
//! These tests verify that:
//!  1. `test_scenarios` populates `total_runtime_secs` with a value that
//!     monotonically reflects the responder's wall-clock cost, so the
//!     CLI's `--budget-secs` warning has a real signal to compare
//!     against.
//!  2. `per_cq_runtime_secs` is parallel to `per_cq` (one entry per CQ).
//!  3. `ScenariosPassResult::budget_exceeded` correctly distinguishes
//!     "exceeded" from "within budget" and from "no measurement"
//!     (legacy results pre-EX-4335).
//!  4. A representative-size mock battery (matching the per-book CQ
//!     count seen in CH-4318 — 27 CQs) completes well inside the
//!     5-minute budget when the responder is fast (deterministic
//!     mock). This is the architectural canary — if this test ever
//!     drifts into the seconds, the orchestrator itself has overhead
//!     we should hunt down.
//!
//! What these tests do NOT cover: the live being+vLLM path. That's a
//! DGX-only check (gated under `#[ignore]` in `v15_floor_replay.rs`).
//! The point of the assertion here is the orchestrator contract, not
//! the model's per-token latency.

use std::time::Duration;

use nusy_evaluator::scenarios::{BeingResponse, DEFAULT_BUDGET_SECS, test_scenarios};
use nusy_evaluator::{Battery, CqSpec, Expect, Grader, GraderConfig};
use nusy_safety::justification::EvidenceTrail;

fn empty_trail() -> EvidenceTrail {
    EvidenceTrail {
        query: String::new(),
        conclusion: String::new(),
        supporting_triples: vec![],
        source_chunks: vec![],
        confidence: 0.0,
        reasoning_path: vec![],
    }
}

fn cq(id: &str, question: &str, dimension: &str) -> CqSpec {
    CqSpec {
        id: id.to_string(),
        question: question.to_string(),
        expected_keywords: vec![],
        expect: Expect::Answer,
        dimension: dimension.to_string(),
        expected_resolution: vec![],
        domain: "general_education".to_string(),
        requirement_id: None,
        scenario_id: None,
        tutor_seal_hash: None,
    }
}

fn battery_of(n: usize, label: &str) -> Battery {
    let cqs: Vec<CqSpec> = (0..n)
        .map(|i| {
            cq(
                &format!("CQ-{:03}", i),
                &format!("question {i}"),
                "word_meaning",
            )
        })
        .collect();
    Battery {
        cqs,
        source_label: label.to_string(),
    }
}

#[test]
fn test_scenarios_populates_total_and_per_cq_runtime() {
    let battery = battery_of(3, "tiny-mock");
    let grader = Grader::new(GraderConfig {
        require_graph_trace: false,
    });

    let result = test_scenarios("mock-being", &battery, &grader, |_cq, _q| BeingResponse {
        response: "An archer uses a bow.".to_string(),
        trail: empty_trail(),
    });

    let total = result
        .total_runtime_secs
        .expect("EX-4335 — total_runtime_secs must be populated");
    assert!(total >= 0.0);
    assert!(total < 60.0, "fast mock responder must finish in <60s");
    assert_eq!(
        result.per_cq_runtime_secs.len(),
        result.per_cq.len(),
        "per_cq_runtime_secs is parallel to per_cq"
    );
    assert_eq!(result.per_cq_runtime_secs.len(), 3);
    for t in &result.per_cq_runtime_secs {
        assert!(*t >= 0.0);
    }
}

#[test]
fn budget_exceeded_returns_false_when_within_budget() {
    let battery = battery_of(2, "within-budget");
    let grader = Grader::new(GraderConfig {
        require_graph_trace: false,
    });
    let result = test_scenarios("mock-being", &battery, &grader, |_cq, _q| BeingResponse {
        response: "Pass content.".to_string(),
        trail: empty_trail(),
    });
    // Mock battery is fast — must come in well under DEFAULT_BUDGET_SECS.
    assert!(!result.budget_exceeded(DEFAULT_BUDGET_SECS));
}

#[test]
fn budget_exceeded_fires_when_runtime_blows_budget() {
    let battery = battery_of(1, "slow-budget-test");
    let grader = Grader::new(GraderConfig {
        require_graph_trace: false,
    });
    let result = test_scenarios("mock-being", &battery, &grader, |_cq, _q| {
        // Sleep 50ms so total_runtime_secs has a non-zero value we can
        // compare against a tiny budget.
        std::thread::sleep(Duration::from_millis(50));
        BeingResponse {
            response: "Some response.".to_string(),
            trail: empty_trail(),
        }
    });
    let total = result.total_runtime_secs.unwrap();
    assert!(total >= 0.05);
    // Tiny budget — must trigger.
    assert!(result.budget_exceeded(0.001));
    // Generous budget — must not trigger.
    assert!(!result.budget_exceeded(60.0));
}

#[test]
fn budget_exceeded_returns_false_on_legacy_result_without_runtime() {
    // Legacy snapshots pre-EX-4335 have `total_runtime_secs: None`.
    // budget_exceeded must NOT crash and must return false (caller can
    // distinguish via `.total_runtime_secs.is_some()`).
    use nusy_evaluator::scenarios::{GradeCounts, ScenariosPassResult};
    let legacy = ScenariosPassResult {
        being_label: "legacy".into(),
        battery_label: "legacy".into(),
        run_at: chrono::Utc::now(),
        per_cq: vec![],
        substantive_pass_rate: 0.0,
        substantive_eligible: 0,
        substantive_passes: 0,
        liberal_pass_rate: 0.0,
        liberal_passes: 0,
        total_cqs: 0,
        gap_list: vec![],
        grade_counts: GradeCounts::default(),
        total_runtime_secs: None,
        per_cq_runtime_secs: vec![],
    };
    assert!(!legacy.budget_exceeded(DEFAULT_BUDGET_SECS));
    assert!(!legacy.budget_exceeded(0.0));
}

#[test]
fn json_round_trip_preserves_runtime_budget_fields() {
    // Schema-stability check — the EX-v dashboard reads these fields,
    // so they must round-trip as documented in the output.rs schema
    // docstring.
    let battery = battery_of(1, "json-rt");
    let grader = Grader::new(GraderConfig {
        require_graph_trace: false,
    });
    let result = test_scenarios("mock", &battery, &grader, |_, _| BeingResponse {
        response: "ok".into(),
        trail: empty_trail(),
    });
    let json = serde_json::to_string(&result).unwrap();
    assert!(json.contains("\"total_runtime_secs\""));
    assert!(json.contains("\"per_cq_runtime_secs\""));

    let parsed: nusy_evaluator::scenarios::ScenariosPassResult =
        serde_json::from_str(&json).unwrap();
    assert_eq!(
        parsed.total_runtime_secs.is_some(),
        result.total_runtime_secs.is_some()
    );
    assert_eq!(parsed.per_cq_runtime_secs.len(), 1);
}

#[test]
fn json_legacy_snapshot_without_runtime_fields_deserializes() {
    // EX-v dashboard ingestion guarantee — a pre-EX-4335 JSON file
    // (no total_runtime_secs / per_cq_runtime_secs keys) must still
    // deserialize. Field-stability rule from output.rs § "JSON schema".
    let legacy_json = r#"{
        "being_label": "santiago-toddler-v15.4",
        "battery_label": "00_alphabet_dame_wonder.expected.md",
        "run_at": "2026-04-01T00:00:00Z",
        "per_cq": [],
        "substantive_pass_rate": 0.0,
        "substantive_eligible": 0,
        "substantive_passes": 0,
        "liberal_pass_rate": 0.0,
        "liberal_passes": 0,
        "total_cqs": 0,
        "gap_list": [],
        "grade_counts": {
            "pass": 0, "fail": 0, "refuse": 0, "persona_leak": 0, "error": 0
        }
    }"#;
    let parsed: nusy_evaluator::scenarios::ScenariosPassResult =
        serde_json::from_str(legacy_json).expect("legacy snapshot must parse");
    assert!(parsed.total_runtime_secs.is_none());
    assert!(parsed.per_cq_runtime_secs.is_empty());
    // budget_exceeded reports false on legacy.
    assert!(!parsed.budget_exceeded(DEFAULT_BUDGET_SECS));
}

#[test]
fn medium_battery_completes_well_within_5min_budget_with_fast_responder() {
    // Architectural canary: 27 CQs (the CH-4318 per-book count) with a
    // free-running responder must finish in well under 5 min. Mini's
    // baseline is sub-second; if this ever creeps into the seconds the
    // test_scenarios orchestrator has overhead worth investigating.
    let battery = battery_of(27, "medium-canary");
    let grader = Grader::new(GraderConfig {
        require_graph_trace: false,
    });
    let result = test_scenarios("mock", &battery, &grader, |_, _| BeingResponse {
        response: "fast mock.".into(),
        trail: empty_trail(),
    });
    let total = result.total_runtime_secs.unwrap();
    assert!(
        total < DEFAULT_BUDGET_SECS,
        "27-CQ mock battery exceeded {DEFAULT_BUDGET_SECS}s budget — orchestrator overhead?"
    );
    // Tighter sanity: a free-running responder on Mini finishes in
    // well under 1s. Even on the slowest CI hardware this should hold
    // by orders of magnitude.
    assert!(
        total < 5.0,
        "27-CQ mock battery took {total:.3}s — expected sub-second; overhead?"
    );
}
