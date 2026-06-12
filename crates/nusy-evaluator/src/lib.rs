//! # nusy-evaluator — Scenarios-pass evaluator (EX-γ / "Taste the Sushi")
//!
//! VY-4312 Phase 1 (Foundation). The evaluator is the closed-loop verification
//! gate that V6 Sushi Pipeline Step 7 introduced and V14/V15 lost. Given a
//! schooled being and a Customer's Plate (tutor record from `nusy-tutor-record`,
//! EX-4332), it grades each curriculum CQ pass / fail / refuse / persona_leak
//! / error and returns the gap list that EX-δ's re-read controller will
//! consume.
//!
//! ## Phase-0 carry-forwards honoured
//!
//! - **A7 (S2):** the grader requires *non-refusal AND keyword AND graph-trace*
//!   — not the bash `run_battery_local.sh` rule (`>20 chars + no refusal`)
//!   that scored 18.5% on a being which substantively answered nothing.
//! - **A10 (S2):** `persona_leak` is a first-class grade alongside
//!   `pass / fail / refuse / error`. The V15.4 persona prompt
//!   ("I am Santiago Ramón…") that leaked on CQ-019 in CH-4318 is now
//!   visible in score summaries.
//! - **A11 (S2):** the legacy CH-4318 `cq_battery.jsonl` shape is a
//!   first-class input format alongside the canonical `TutorRecord`.
//! - **D1 (LIT-A § 4.2):** scenarios-pass-rate is a stricter signal than
//!   retrieval-recall. The grader's `pass` discipline is what makes that
//!   stricter.
//! - **D4 (LIT-A § 4.2):** every `Pass` carries a provenance chain
//!   (`Vec<TripleRef>`) tagged by Y-layer. The chain is harvested from the
//!   Being's `EvidenceTrail` (`nusy-safety::justification`); EX-α's
//!   per-triple chunk-id tagging will populate this richly when it lands —
//!   until then the chain is the EvidenceTrail's existing supporting-triple
//!   set, which is the V15-era best-effort.
//!
//! ## What this crate does NOT do
//!
//! - Does **not** drive the Being directly. The caller invokes
//!   `Being::chat(...)` (or any backend) and hands the response + trail to
//!   `Grader`. This decouples the evaluator from GPU-only Candle paths so
//!   `cargo test -p nusy-evaluator` runs on Mini.
//! - Does **not** assume EX-α has landed. Provenance plumbing is in place,
//!   so EX-α's richer cortex-side tagging will compose without API change.

pub mod battery;
pub mod dashboard;
pub mod grader;
pub mod output;
pub mod provenance;
pub mod regression;
pub mod scenarios;
pub mod strict; // CH-4442: strict triple-match grader.

pub use battery::{Battery, BatteryError, CqSpec, Expect, ExpectedTriple};
pub use dashboard::{
    BeingTrend, CqFailureStats, DashboardError, DashboardModel, LatestRun, TrendPoint, build_model,
    load_results_from_dir, render_html, render_markdown,
};
pub use grader::{Grade, Grader, GraderConfig, GraderError};
pub use output::{ProvenanceSummary, write_json_report, write_markdown_report};
pub use provenance::{PassResult, TripleRef};
pub use regression::{
    DEFAULT_THRESHOLD_PP, RegressionAlert, RegressionPair, RegressionReport, compare,
    render_markdown as render_regression_markdown,
};
pub use scenarios::{ScenariosPassResult, test_scenarios};
pub use strict::{
    STRICT_PASS_THRESHOLD, StrictBatteryReport, StrictGradeReport, TripleMatch,
    grade_strict_battery, grade_strict_cq,
};
