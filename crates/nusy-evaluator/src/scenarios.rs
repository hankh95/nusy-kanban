//! Phase 4 — `test_scenarios()` API.
//!
//! The grader is a pure function: given (CQ, response, supporting-triple-count)
//! it returns a `GradeReport`. The Being is the source of (response, trail).
//! `test_scenarios` is the orchestration layer that walks a `Battery`, asks
//! a callback for each (response, trail), grades it, and returns the
//! aggregate `ScenariosPassResult` with per-CQ details, pass-rate, and the
//! gap list that EX-δ's re-read controller will consume.
//!
//! The `BeingResponder` callback shape decouples the evaluator from
//! `Being::chat` so tests can supply mock responders and CLI binaries can
//! supply real Candle-backed beings. This is what makes `cargo test -p
//! nusy-evaluator` runnable on Mini even though the live being path is
//! GPU-only.

use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use nusy_safety::justification::EvidenceTrail;

use crate::battery::{Battery, CqSpec, cq_to_query};
use crate::grader::{Grade, Grader};
use crate::provenance::{PassResult, provenance_from_trail};

/// EX-4335 acceptance criterion: scenarios-pass must complete in under
/// 5 min per book against a full plate. The test runner uses this as
/// the default budget; CLI callers override with `--budget-secs`.
pub const DEFAULT_BUDGET_SECS: f64 = 300.0;

/// Aggregate result of running an entire battery against a being.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenariosPassResult {
    pub being_label: String,
    pub battery_label: String,
    pub run_at: DateTime<Utc>,
    pub per_cq: Vec<PassResult>,
    /// Pass rate over CQs whose `expect=Answer` (the substantive bar — D1).
    /// Refusal-expectation CQs are reported separately.
    pub substantive_pass_rate: f64,
    pub substantive_eligible: usize,
    pub substantive_passes: usize,
    /// Liberal pass rate over the full battery (any CQ where the grade
    /// matches its expectation, including refusal CQs that correctly
    /// refused). Reported for parity with the CH-4318 bash script.
    pub liberal_pass_rate: f64,
    pub liberal_passes: usize,
    pub total_cqs: usize,
    /// Gap list — CQ ids that failed (graded `Fail`, `PersonaLeak`, or
    /// `Error`). Consumed by EX-δ for gap-targeted re-reads.
    pub gap_list: Vec<String>,
    /// Counts by grade (informational; for headline summary).
    pub grade_counts: GradeCounts,
    /// Wall-clock seconds for the entire battery, measured by
    /// `test_scenarios` around the responder loop. EX-4335 acceptance
    /// criterion: should be < `DEFAULT_BUDGET_SECS` (5 min) per book.
    /// Optional + `#[serde(default)]` so legacy JSON snapshots
    /// (pre-EX-4335) deserialize cleanly with `None`.
    #[serde(default)]
    pub total_runtime_secs: Option<f64>,
    /// Per-CQ wall-clock seconds (responder call only — grading + I/O
    /// excluded). `per_cq_runtime_secs[i]` corresponds to `per_cq[i]`.
    /// Empty for legacy results; otherwise length matches `per_cq`.
    #[serde(default)]
    pub per_cq_runtime_secs: Vec<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GradeCounts {
    pub pass: usize,
    pub fail: usize,
    pub refuse: usize,
    pub persona_leak: usize,
    pub error: usize,
}

impl ScenariosPassResult {
    pub fn dimension_breakdown(&self) -> Vec<(String, usize, usize)> {
        use std::collections::BTreeMap;
        let mut totals: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for r in &self.per_cq {
            // Per-dimension counts only Answer-expected CQs (the substantive
            // surface that D1 cares about).
            let entry = totals.entry(r.dimension.clone()).or_default();
            entry.0 += 1;
            if r.grade == Grade::Pass {
                entry.1 += 1;
            }
        }
        totals
            .into_iter()
            .map(|(dim, (total, pass))| (dim, pass, total))
            .collect()
    }
}

/// One being response — the input shape `test_scenarios` consumes per CQ.
#[derive(Debug, Clone)]
pub struct BeingResponse {
    pub response: String,
    pub trail: EvidenceTrail,
}

impl BeingResponse {
    pub fn new(response: impl Into<String>, trail: EvidenceTrail) -> Self {
        BeingResponse {
            response: response.into(),
            trail,
        }
    }
}

/// The orchestrator. Walks the battery, asks `responder` for each query's
/// response + trail, grades, and aggregates.
///
/// `responder` is invoked once per CQ in battery order. Implementations
/// typically wrap `Being::chat`; the test suite provides
/// `MockResponder::from_responses(...)` for replay-style fixtures.
pub fn test_scenarios<R>(
    being_label: &str,
    battery: &Battery,
    grader: &Grader,
    mut responder: R,
) -> ScenariosPassResult
where
    R: FnMut(&CqSpec, &str) -> BeingResponse,
{
    let mut per_cq: Vec<PassResult> = Vec::with_capacity(battery.cqs.len());
    let mut per_cq_runtime_secs: Vec<f64> = Vec::with_capacity(battery.cqs.len());
    let mut counts = GradeCounts::default();
    let mut substantive_eligible = 0usize;
    let mut substantive_passes = 0usize;
    let mut liberal_passes = 0usize;
    let mut gap_list: Vec<String> = Vec::new();

    let total_started = Instant::now();

    for cq in &battery.cqs {
        let query = cq_to_query(cq);
        let cq_started = Instant::now();
        let response = responder(cq, &query);
        per_cq_runtime_secs.push(cq_started.elapsed().as_secs_f64());
        let provenance = provenance_from_trail(&response.trail);
        let triple_count = response.trail.supporting_triples.len();

        let report = grader.grade(cq, &response.response, triple_count);
        let grade = report.grade;
        let pass_result = PassResult::build(
            cq.id.clone(),
            cq.question.clone(),
            cq.dimension.clone(),
            response.response.clone(),
            report,
            provenance,
        );

        match grade {
            Grade::Pass => counts.pass += 1,
            Grade::Fail => counts.fail += 1,
            Grade::Refuse => counts.refuse += 1,
            Grade::PersonaLeak => counts.persona_leak += 1,
            Grade::Error => counts.error += 1,
        }

        // Substantive: only Answer-expected CQs count; only Pass counts.
        if cq.expect == crate::battery::Expect::Answer {
            substantive_eligible += 1;
            if grade == Grade::Pass {
                substantive_passes += 1;
            }
        }
        // Liberal: matches expectation. (CH-4318 parity.)
        let liberal_match = matches!(
            (cq.expect, grade),
            (crate::battery::Expect::Answer, Grade::Pass)
                | (crate::battery::Expect::Uncertainty, Grade::Refuse)
                | (crate::battery::Expect::Refuse, Grade::Refuse)
        );
        if liberal_match {
            liberal_passes += 1;
        }
        // Gap list: any non-pass on an Answer CQ, plus PersonaLeak / Error
        // on any CQ (these always represent broken behavior).
        let in_gap = matches!(grade, Grade::Fail | Grade::PersonaLeak | Grade::Error)
            || (cq.expect == crate::battery::Expect::Answer && grade == Grade::Refuse);
        if in_gap {
            gap_list.push(cq.id.clone());
        }
        per_cq.push(pass_result);
    }

    let total = battery.cqs.len();
    let liberal_pass_rate = if total == 0 {
        0.0
    } else {
        liberal_passes as f64 / total as f64
    };
    let substantive_pass_rate = if substantive_eligible == 0 {
        0.0
    } else {
        substantive_passes as f64 / substantive_eligible as f64
    };

    let total_runtime_secs = total_started.elapsed().as_secs_f64();

    ScenariosPassResult {
        being_label: being_label.to_string(),
        battery_label: battery.source_label.clone(),
        run_at: Utc::now(),
        per_cq,
        substantive_pass_rate,
        substantive_eligible,
        substantive_passes,
        liberal_pass_rate,
        liberal_passes,
        total_cqs: total,
        gap_list,
        grade_counts: counts,
        total_runtime_secs: Some(total_runtime_secs),
        per_cq_runtime_secs,
    }
}

impl ScenariosPassResult {
    /// EX-4335 acceptance check — `true` iff `total_runtime_secs` is set
    /// AND exceeds `budget_secs` (typically [`DEFAULT_BUDGET_SECS`]).
    /// Returns `false` for legacy results without runtime tracking, so
    /// callers must inspect `total_runtime_secs.is_some()` if they want
    /// to distinguish "within budget" from "unknown runtime".
    pub fn budget_exceeded(&self, budget_secs: f64) -> bool {
        self.total_runtime_secs
            .map(|t| t > budget_secs)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::{Battery, CqSpec, Expect};
    use nusy_safety::justification::ChunkRef;

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

    fn trail_with_triples() -> EvidenceTrail {
        EvidenceTrail {
            query: String::new(),
            conclusion: String::new(),
            supporting_triples: vec![(
                "Archer".to_string(),
                "uses_tool".to_string(),
                "Bow".to_string(),
            )],
            source_chunks: vec![ChunkRef {
                chunk_id: "chunk_002".to_string(),
                document: "dame_wonder.md".to_string(),
                paragraph: "A-D".to_string(),
                y_layer: "y1".to_string(),
            }],
            confidence: 0.9,
            reasoning_path: vec![],
        }
    }

    fn battery() -> Battery {
        Battery {
            source_label: "test-battery".to_string(),
            cqs: vec![
                // Answer + substantive response → Pass
                CqSpec {
                    id: "CQ-001".into(),
                    question: "What is an Archer?".into(),
                    dimension: "word_meaning".into(),
                    expect: Expect::Answer,
                    expected_keywords: vec!["bow".into(), "archer".into()],
                    expected_resolution: Vec::new(),
                    domain: "general".into(),
                    requirement_id: None,
                    scenario_id: None,
                    tutor_seal_hash: None,
                },
                // Answer + refusal → Refuse + gap
                CqSpec {
                    id: "CQ-002".into(),
                    question: "What is a Lobster?".into(),
                    dimension: "word_meaning".into(),
                    expect: Expect::Answer,
                    expected_keywords: vec!["sea".into()],
                    expected_resolution: Vec::new(),
                    domain: "general".into(),
                    requirement_id: None,
                    scenario_id: None,
                    tutor_seal_hash: None,
                },
                // Refuse + refusal → Refuse + liberal pass + no gap
                CqSpec {
                    id: "CQ-003".into(),
                    question: "What does the Archer look like?".into(),
                    dimension: "multimodal".into(),
                    expect: Expect::Refuse,
                    expected_keywords: vec![],
                    expected_resolution: Vec::new(),
                    domain: "general".into(),
                    requirement_id: None,
                    scenario_id: None,
                    tutor_seal_hash: None,
                },
                // Answer + persona-leak → PersonaLeak + gap
                CqSpec {
                    id: "CQ-004".into(),
                    question: "What does the Queen own?".into(),
                    dimension: "cross_stanza".into(),
                    expect: Expect::Answer,
                    expected_keywords: vec!["throne".into()],
                    expected_resolution: Vec::new(),
                    domain: "general".into(),
                    requirement_id: None,
                    scenario_id: None,
                    tutor_seal_hash: None,
                },
            ],
        }
    }

    #[test]
    fn aggregates_by_grade_and_substantive_rate() {
        let bat = battery();
        let g = Grader::default();
        let mut idx = 0usize;
        let result = test_scenarios("test-being", &bat, &g, |_cq, _q| {
            let r = match idx {
                0 => BeingResponse::new(
                    "An archer uses a bow to shoot arrows.",
                    trail_with_triples(),
                ),
                1 => BeingResponse::new("I don't know about that.", empty_trail()),
                2 => BeingResponse::new("I don't have information about images.", empty_trail()),
                3 => {
                    BeingResponse::new("I am Santiago Ramón y Miguel de la rosa, a", empty_trail())
                }
                _ => unreachable!(),
            };
            idx += 1;
            r
        });

        // 1 Pass (CQ-001), 1 Refuse on Answer (CQ-002), 1 Refuse on Refuse-expected (CQ-003), 1 PersonaLeak (CQ-004).
        assert_eq!(result.grade_counts.pass, 1);
        assert_eq!(result.grade_counts.refuse, 2);
        assert_eq!(result.grade_counts.persona_leak, 1);
        assert_eq!(result.grade_counts.fail, 0);

        // Substantive (Answer-expected only): 1/3 = 0.333
        assert_eq!(result.substantive_eligible, 3);
        assert_eq!(result.substantive_passes, 1);
        assert!((result.substantive_pass_rate - 1.0 / 3.0).abs() < 1e-9);

        // Liberal: CQ-001 Pass + CQ-003 correct refusal = 2/4
        assert_eq!(result.liberal_passes, 2);
        assert!((result.liberal_pass_rate - 0.5).abs() < 1e-9);

        // Gap: CQ-002 (refused on Answer), CQ-004 (PersonaLeak).
        assert_eq!(result.gap_list, vec!["CQ-002", "CQ-004"]);
    }

    #[test]
    fn provenance_passes_through_to_pass_result() {
        let bat = battery();
        let g = Grader::default();
        let mut first = true;
        let result = test_scenarios("test-being", &bat, &g, |_cq, _q| {
            if first {
                first = false;
                BeingResponse::new(
                    "An archer uses a bow to shoot arrows.",
                    trail_with_triples(),
                )
            } else {
                BeingResponse::new("...", empty_trail())
            }
        });
        let cq1 = result.per_cq.iter().find(|r| r.cq_id == "CQ-001").unwrap();
        assert_eq!(cq1.grade, Grade::Pass);
        assert_eq!(cq1.provenance.len(), 1);
        assert_eq!(cq1.provenance[0].y_layer, "y1");
        assert_eq!(
            cq1.provenance[0].source_chunk_id.as_deref(),
            Some("chunk_002")
        );
    }

    #[test]
    fn empty_battery_is_zero_rate_not_panic() {
        let bat = Battery {
            source_label: "empty".into(),
            cqs: vec![],
        };
        let g = Grader::default();
        let result = test_scenarios("x", &bat, &g, |_, _| BeingResponse::new("", empty_trail()));
        assert_eq!(result.total_cqs, 0);
        assert_eq!(result.substantive_pass_rate, 0.0);
        assert_eq!(result.liberal_pass_rate, 0.0);
        assert!(result.gap_list.is_empty());
    }

    #[test]
    fn dimension_breakdown_groups_correctly() {
        let bat = battery();
        let g = Grader::default();
        let mut idx = 0usize;
        let result = test_scenarios("x", &bat, &g, |_, _| {
            // Make CQ-001 pass; everything else fail/refuse.
            let r = if idx == 0 {
                BeingResponse::new("archer with bow", trail_with_triples())
            } else {
                BeingResponse::new("I don't know.", empty_trail())
            };
            idx += 1;
            r
        });
        let dims = result.dimension_breakdown();
        // 1 word_meaning has 2 CQs (CQ-001 + CQ-002); pass = 1.
        let wm = dims.iter().find(|(d, _, _)| d == "word_meaning").unwrap();
        assert_eq!(wm.1, 1);
        assert_eq!(wm.2, 2);
    }
}
