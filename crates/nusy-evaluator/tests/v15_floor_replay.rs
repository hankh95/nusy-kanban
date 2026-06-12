//! Phase 6 integration — V15 floor reproduction (CH-4318 replay).
//!
//! This test replays the CH-4318 V15.4 toddler measurement: the actual
//! response strings the schooled being produced are stored in
//! `research/shared/eval-data/expr-vy-v16/ch-4318/results_*.jsonl`. Running
//! EX-γ's grader over those replayed responses reproduces the V15 floor
//! Mini / DGX measured live in S2 — and additionally proves EX-γ's stricter
//! grader catches the V15.4 persona-leak (A10 carry-forward) that
//! CH-4318's bash script labelled as `fail` instead of as a distinct class.
//!
//! Why replay rather than live being-driven test:
//! - The actual V15.4 LoRA inference path requires Candle + GPU. Per
//!   CLAUDE.md GPU-first architecture, training / inference / schooling
//!   run exclusively on DGX. This replay test runs on Mini.
//! - The live being-driven test (`#[ignore]`-gated below) is the same
//!   `test_scenarios()` flow with a real `Being::chat` responder. EX-α's
//!   landing will make it relevant; until then it's marker code so the
//!   harness shape is locked in.

use std::collections::HashMap;
use std::path::PathBuf;

use nusy_evaluator::scenarios::BeingResponse;
use nusy_evaluator::{Battery, Grade, Grader, GraderConfig, test_scenarios};
use nusy_safety::justification::EvidenceTrail;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct V15Result {
    id: String,
    response: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn load_v15_results(path: &PathBuf) -> HashMap<String, String> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = HashMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: V15Result = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{} line {}: {e}", path.display(), i + 1));
        out.insert(row.id, row.response);
    }
    out
}

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

/// V15 path produces no graph-trace surfacing. Disable the graph-trace
/// requirement so the test reproduces CH-4318's measurement exactly —
/// non-refusal + keyword is the bar V15 was being graded on by the bash
/// script, plus EX-γ's stricter persona-leak detection (A10).
fn v15_grader() -> Grader {
    Grader::new(GraderConfig {
        require_graph_trace: false,
    })
}

#[test]
fn v15_floor_reproduces_zero_substantive_on_graph_only_path() {
    let battery_path = repo_root().join("scripts/ch-4318/cq_battery.jsonl");
    let results_path = repo_root().join(
        "research/shared/eval-data/expr-vy-v16/ch-4318/results_santiago-toddler-v15.4_cq_battery_20260504_013747.jsonl",
    );
    if !battery_path.exists() || !results_path.exists() {
        eprintln!("skipping: CH-4318 fixtures missing in this checkout");
        return;
    }

    let battery = Battery::load(&battery_path).expect("battery loads");
    let responses = load_v15_results(&results_path);
    let grader = v15_grader();

    let result = test_scenarios(
        "santiago-toddler-v15.4 (graph-only)",
        &battery,
        &grader,
        |cq, _q| {
            let response = responses.get(&cq.id).cloned().unwrap_or_default();
            BeingResponse::new(response, empty_trail())
        },
    );

    // CH-4318's headline number: 0 / 22 substantive on Answer-expected CQs.
    assert_eq!(
        result.substantive_passes, 0,
        "V15 graph-only path must reproduce 0/22 substantive (CH-4318 floor)"
    );
    // The 22 expect=answer CQs in cq_battery.jsonl
    assert_eq!(result.substantive_eligible, 22);
    assert_eq!(result.substantive_pass_rate, 0.0);

    // Liberal pass count should be 5 — same as CH-4318 (the 5 refusal-expectation
    // matches CQ-007/CQ-010/CQ-022/CQ-023/CQ-024).
    assert_eq!(
        result.liberal_passes, 5,
        "graph-only liberal pass-rate should be 5/27 = 18.5% per CH-4318"
    );

    // Total CQs in the battery is 27.
    assert_eq!(result.total_cqs, 27);

    // No persona-leaks on the graph-only path (template refusals only).
    assert_eq!(result.grade_counts.persona_leak, 0);
}

#[test]
fn v15_floor_llm_on_path_detects_persona_leak() {
    let battery_path = repo_root().join("scripts/ch-4318/cq_battery.jsonl");
    let results_path = repo_root().join(
        "research/shared/eval-data/expr-vy-v16/ch-4318/results_santiago-toddler-v15.4_cq_battery_20260504_013838.jsonl",
    );
    if !battery_path.exists() || !results_path.exists() {
        eprintln!("skipping: CH-4318 fixtures missing in this checkout");
        return;
    }

    let battery = Battery::load(&battery_path).expect("battery loads");
    let responses = load_v15_results(&results_path);
    let grader = v15_grader();

    let result = test_scenarios(
        "santiago-toddler-v15.4 (llm-on)",
        &battery,
        &grader,
        |cq, _q| {
            let response = responses.get(&cq.id).cloned().unwrap_or_default();
            BeingResponse::new(response, empty_trail())
        },
    );

    // Same headline: 0 / 22 substantive.
    assert_eq!(result.substantive_passes, 0);
    assert_eq!(result.substantive_eligible, 22);

    // EX-γ A10 win: at least one persona_leak grade (CQ-010 in the LLM-on
    // run leaked the LoRA's persona prefix). CH-4318's bash grader had this
    // labelled `fail`; EX-γ surfaces it as a distinct class.
    assert!(
        result.grade_counts.persona_leak >= 1,
        "EX-γ must catch the V15.4 persona-leak that CH-4318's grader missed; \
         got persona_leak={}",
        result.grade_counts.persona_leak
    );

    // The leaked CQ should appear in the gap list (PersonaLeak always does).
    assert!(
        result
            .per_cq
            .iter()
            .any(|cq| cq.grade == Grade::PersonaLeak),
        "at least one CQ should have grade=PersonaLeak"
    );
}

#[test]
fn v15_floor_dimension_breakdown_zero_per_dimension() {
    let battery_path = repo_root().join("scripts/ch-4318/cq_battery.jsonl");
    let results_path = repo_root().join(
        "research/shared/eval-data/expr-vy-v16/ch-4318/results_santiago-toddler-v15.4_cq_battery_20260504_013747.jsonl",
    );
    if !battery_path.exists() || !results_path.exists() {
        eprintln!("skipping: CH-4318 fixtures missing in this checkout");
        return;
    }

    let battery = Battery::load(&battery_path).expect("battery loads");
    let responses = load_v15_results(&results_path);
    let grader = v15_grader();
    let result = test_scenarios(
        "santiago-toddler-v15.4 (graph-only, per-dim)",
        &battery,
        &grader,
        |cq, _q| {
            let response = responses.get(&cq.id).cloned().unwrap_or_default();
            BeingResponse::new(response, empty_trail())
        },
    );

    // CH-4318's per-dimension table shows 0/X for every dimension.
    for (dim, pass, total) in result.dimension_breakdown() {
        assert_eq!(
            pass, 0,
            "expected 0 substantive passes in {dim} ({pass}/{total} actual)"
        );
        assert!(total > 0, "every dimension should have at least 1 CQ");
    }
}

#[test]
#[ignore = "requires schooled santiago-toddler-v15.4 + Candle GPU; run on DGX"]
fn live_v15_floor_runs_on_dgx_only() {
    // Marker test — when EX-α lands and the run-on-DGX harness is wired up,
    // this is the entry point for the live-being version of v15_floor_reproduces.
    // Until then the replay tests above are the on-Mini equivalent.
    panic!(
        "not implemented — see v15_floor_reproduces_zero_substantive_on_graph_only_path for the replay version"
    );
}

#[test]
#[ignore = "requires EX-α cortex API + schooled cortex-being; run on DGX after EX-α lands"]
fn live_cortex_path_phase_1_target() {
    // Marker test — once EX-α produces an answer-shaped output with provenance
    // (A8), this test schools a cortex-being on the same Dame Wonder corpus,
    // runs EX-γ, and asserts substantive_pass_rate >= 0.30 (the Phase-1
    // readiness check; the final >= 0.70 lives at EX-ι Phase 4).
    panic!("not implemented — depends on EX-α cortex API");
}
