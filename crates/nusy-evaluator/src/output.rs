//! Phase 5 — output formats.
//!
//! Three writers:
//!
//! 1. **Markdown report** — same shape as
//!    `research/shared/eval-data/expr-vy-v16/spike-s2-v15-floor.md` (the
//!    bash-script per-dimension report from CH-4318), so the scenarios-pass
//!    output is byte-comparable to the V15 floor capture.
//! 2. **JSON report** — full `ScenariosPassResult` serialised; consumed by
//!    Mini's metrics dashboard (VY-4313 EX-v).
//! 3. **Per-Y-layer provenance summary** — D4 audit support: how many
//!    passes leaned on Y0 vs Y1 vs Y2 evidence, and which CQs lacked any
//!    provenance at all.
//!
//! # JSON schema (EX-4335 polish — for EX-v / EX-4336 dashboard ingestion)
//!
//! `write_json_report` serialises [`ScenariosPassResult`] with
//! `serde_json::to_writer_pretty` plus a trailing newline. The shape is
//! stable — adding non-breaking fields uses `#[serde(default)]` so older
//! consumers continue to deserialize. **Top-level keys:**
//!
//! | Key | Type | Notes |
//! |---|---|---|
//! | `being_label` | `string` | E.g. `"santiago-toddler-v15.4"`. Unique per being+version. |
//! | `battery_label` | `string` | Source of the plate, e.g. `"00_alphabet_dame_wonder.expected.md"`. |
//! | `run_at` | `string` (RFC3339 UTC) | When `test_scenarios` started. |
//! | `total_cqs` | `integer ≥ 0` | Number of CQs in the battery. |
//! | `substantive_pass_rate` | `number` (0..=1) | D1 — Pass over `Expect::Answer` CQs only. **Headline metric.** |
//! | `substantive_eligible` | `integer` | Denominator of `substantive_pass_rate`. |
//! | `substantive_passes` | `integer` | Numerator of `substantive_pass_rate`. |
//! | `liberal_pass_rate` | `number` (0..=1) | CH-4318 parity — any expected outcome. |
//! | `liberal_passes` | `integer` | Numerator of `liberal_pass_rate`. |
//! | `gap_list` | `array<string>` | CQ ids that are `Fail`/`PersonaLeak`/`Error` or refused an Answer-expected CQ. Consumed by EX-δ re-read controller. |
//! | `grade_counts` | `object` | `{pass, fail, refuse, persona_leak, error}` — each `integer ≥ 0`. |
//! | `total_runtime_secs` | `number \| null` | EX-4335 acceptance signal — wall-clock for the responder loop. `null` on legacy results. |
//! | `per_cq_runtime_secs` | `array<number>` | Per-CQ wall-clock; same length as `per_cq` when populated. Empty on legacy results. |
//! | `per_cq` | `array<PassResult>` | One entry per CQ in battery order. |
//!
//! **`PassResult` shape:**
//!
//! | Key | Type | Notes |
//! |---|---|---|
//! | `cq_id` | `string` | Stable id from the plate. |
//! | `question` | `string` | The natural-language CQ. |
//! | `dimension` | `string` | Curriculum dimension (e.g. `"word_meaning"`). |
//! | `response` | `string` | Being's chat output. |
//! | `grade` | `string` | One of `"pass" / "fail" / "refuse" / "persona_leak" / "error"`. |
//! | `matched_keywords` | `array<string>` | Pattern hits the grader counted. |
//! | `graph_trace_present` | `bool` | Whether the EvidenceTrail had ≥1 supporting triple (A7 condition). |
//! | `provenance` | `array<TripleRef>` | D4 — supporting triples, tagged by Y-layer. |
//!
//! **Field-stability rule for EX-v:** the dashboard should treat any new
//! key as additive and present-but-null fields as legacy snapshots.
//! Renames or type changes require a new field name + bumped schema
//! version comment in this docstring.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::grader::Grade;
use crate::scenarios::ScenariosPassResult;

/// Per-Y-layer roll-up of the provenance chains. Useful when EX-α lands —
/// a healthy Phase-1 cortex run should show Y0 + Y1 + Y2 represented;
/// all-Y0 means the being is repeating prose, all-Y2 means the cortex
/// reached for reasoning without literal grounding.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProvenanceSummary {
    /// Y-layer string → number of triples from passes that cited it.
    pub triples_by_y_layer: BTreeMap<String, usize>,
    /// Number of passes that had ZERO provenance triples (degraded mode).
    pub passes_without_provenance: usize,
    /// Number of passes that had at least one provenance triple.
    pub passes_with_provenance: usize,
    /// CQ ids of passes with no provenance — surface for review.
    pub passes_without_provenance_ids: Vec<String>,
}

impl ProvenanceSummary {
    pub fn from_result(result: &ScenariosPassResult) -> Self {
        let mut summary = ProvenanceSummary::default();
        for cq in &result.per_cq {
            if cq.grade != Grade::Pass {
                continue;
            }
            if cq.provenance.is_empty() {
                summary.passes_without_provenance += 1;
                summary.passes_without_provenance_ids.push(cq.cq_id.clone());
            } else {
                summary.passes_with_provenance += 1;
                for triple in &cq.provenance {
                    *summary
                        .triples_by_y_layer
                        .entry(triple.y_layer.clone())
                        .or_insert(0) += 1;
                }
            }
        }
        summary
    }
}

/// Render the run as a Markdown report. Layout mirrors the CH-4318
/// per-dimension table the V15 floor-capture used.
pub fn render_markdown(result: &ScenariosPassResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Scenarios-pass report — {}", result.being_label);
    let _ = writeln!(out);
    let _ = writeln!(out, "**Battery:** `{}`  ", result.battery_label);
    let _ = writeln!(
        out,
        "**Run at:** {}  ",
        result.run_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    let _ = writeln!(out, "**Total CQs:** {}", result.total_cqs);
    let _ = writeln!(out);

    // Headline numbers.
    let _ = writeln!(out, "## Headline");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Metric | Hits | Total | Rate |");
    let _ = writeln!(out, "|---|---:|---:|---:|");
    let _ = writeln!(
        out,
        "| **Substantive (D1)** | {} | {} | **{:.1}%** |",
        result.substantive_passes,
        result.substantive_eligible,
        result.substantive_pass_rate * 100.0
    );
    let _ = writeln!(
        out,
        "| Liberal (CH-4318 parity) | {} | {} | {:.1}% |",
        result.liberal_passes,
        result.total_cqs,
        result.liberal_pass_rate * 100.0
    );
    let _ = writeln!(out);

    // EX-4335 acceptance signal — wall-clock vs budget.
    if let Some(secs) = result.total_runtime_secs {
        let budget = crate::scenarios::DEFAULT_BUDGET_SECS;
        let status = if result.budget_exceeded(budget) {
            format!("**OVER** ({:.1}× budget)", secs / budget)
        } else {
            "within".to_string()
        };
        let _ = writeln!(out, "## Runtime");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Total: **{:.1}s** / {:.0}s budget — {}.",
            secs, budget, status
        );
        let _ = writeln!(out);
    }

    // Per-grade counts.
    let _ = writeln!(out, "## Grade counts");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Grade | Count |");
    let _ = writeln!(out, "|---|---:|");
    let _ = writeln!(out, "| pass | {} |", result.grade_counts.pass);
    let _ = writeln!(out, "| fail | {} |", result.grade_counts.fail);
    let _ = writeln!(out, "| refuse | {} |", result.grade_counts.refuse);
    let _ = writeln!(
        out,
        "| persona_leak | {} |",
        result.grade_counts.persona_leak
    );
    let _ = writeln!(out, "| error | {} |", result.grade_counts.error);
    let _ = writeln!(out);

    // Per-dimension substantive breakdown.
    let _ = writeln!(out, "## Per-dimension substantive rate");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Dimension | Pass | Total | Rate |");
    let _ = writeln!(out, "|---|---:|---:|---:|");
    for (dim, pass, total) in result.dimension_breakdown() {
        let rate = if total == 0 {
            0.0
        } else {
            pass as f64 / total as f64 * 100.0
        };
        let _ = writeln!(out, "| {dim} | {pass} | {total} | {rate:.1}% |");
    }
    let _ = writeln!(out);

    // Y-layer provenance summary.
    let prov = ProvenanceSummary::from_result(result);
    let _ = writeln!(out, "## Provenance (D4 audit)");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Passes with provenance: **{}**  ",
        prov.passes_with_provenance
    );
    let _ = writeln!(
        out,
        "Passes without provenance (degraded mode pre-EX-α or empty trail): **{}**  ",
        prov.passes_without_provenance
    );
    if !prov.triples_by_y_layer.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "| Y-layer | Triples cited by passes |");
        let _ = writeln!(out, "|---|---:|");
        for (y, count) in &prov.triples_by_y_layer {
            let _ = writeln!(out, "| {y} | {count} |");
        }
    }
    let _ = writeln!(out);

    // Gap list.
    let _ = writeln!(out, "## Gap list (consumed by EX-δ re-read controller)");
    let _ = writeln!(out);
    if result.gap_list.is_empty() {
        let _ = writeln!(out, "_(none — all Answer CQs passed)_");
    } else {
        for cq_id in &result.gap_list {
            let _ = writeln!(out, "- `{cq_id}`");
        }
    }
    let _ = writeln!(out);

    // Per-CQ details.
    let _ = writeln!(out, "## Per-CQ detail");
    let _ = writeln!(out);
    for cq in &result.per_cq {
        let _ = writeln!(
            out,
            "### `{}` — {} ({})",
            cq.cq_id,
            cq.grade.label(),
            cq.dimension
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "**Q:** {}", cq.question);
        let _ = writeln!(out);
        let _ = writeln!(out, "**Response:**");
        let _ = writeln!(out);
        let _ = writeln!(out, "> {}", cq.response.replace('\n', "\n> "));
        let _ = writeln!(out);
        if !cq.matched_keywords.is_empty() {
            let _ = writeln!(
                out,
                "**Matched keywords:** {}",
                cq.matched_keywords.join(", ")
            );
            let _ = writeln!(out);
        }
        if !cq.provenance.is_empty() {
            let _ = writeln!(out, "**Provenance:**");
            let _ = writeln!(out);
            for t in &cq.provenance {
                let _ = writeln!(
                    out,
                    "- ({}, {}, {}) — {}{}",
                    t.subject,
                    t.predicate,
                    t.object,
                    t.y_layer,
                    t.source_chunk_id
                        .as_deref()
                        .map(|c| format!(", chunk={c}"))
                        .unwrap_or_default(),
                );
            }
            let _ = writeln!(out);
        }
    }

    out
}

/// Write the Markdown report to a file.
pub fn write_markdown_report(
    result: &ScenariosPassResult,
    path: impl AsRef<Path>,
) -> std::io::Result<()> {
    let body = render_markdown(result);
    std::fs::write(path, body)
}

/// Write the full result as JSON.
pub fn write_json_report(
    result: &ScenariosPassResult,
    path: impl AsRef<Path>,
) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(&mut f, result)?;
    f.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grader::GradeReport;
    use crate::provenance::{PassResult, TripleRef};
    use crate::scenarios::GradeCounts;
    use chrono::{TimeZone, Utc};

    fn fixture_result() -> ScenariosPassResult {
        let report = GradeReport {
            cq_id: "CQ-001".to_string(),
            grade: Grade::Pass,
            matched_keywords: vec!["bow".to_string()],
            graph_trace_present: true,
            refusal_signal: false,
            persona_leak_signal: false,
        };
        let pr = PassResult::build(
            "CQ-001".to_string(),
            "What is an Archer?".to_string(),
            "word_meaning".to_string(),
            "An archer uses a bow.".to_string(),
            report,
            vec![TripleRef {
                subject: "Archer".to_string(),
                predicate: "uses_tool".to_string(),
                object: "Bow".to_string(),
                y_layer: "y1".to_string(),
                source_chunk_id: Some("chunk_002".to_string()),
                source_document: Some("dame_wonder.md".to_string()),
            }],
        );
        ScenariosPassResult {
            being_label: "test-being".to_string(),
            battery_label: "test-battery".to_string(),
            run_at: Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
            per_cq: vec![pr],
            substantive_pass_rate: 1.0,
            substantive_eligible: 1,
            substantive_passes: 1,
            liberal_pass_rate: 1.0,
            liberal_passes: 1,
            total_cqs: 1,
            gap_list: vec![],
            grade_counts: GradeCounts {
                pass: 1,
                ..Default::default()
            },
            total_runtime_secs: Some(0.42),
            per_cq_runtime_secs: vec![0.42],
        }
    }

    #[test]
    fn markdown_report_contains_headline_substantive_rate() {
        let r = fixture_result();
        let md = render_markdown(&r);
        assert!(md.contains("**Substantive (D1)**"));
        assert!(md.contains("100.0%"));
    }

    #[test]
    fn markdown_report_includes_provenance_table() {
        let r = fixture_result();
        let md = render_markdown(&r);
        assert!(md.contains("Provenance (D4 audit)"));
        assert!(md.contains("y1"));
        assert!(md.contains("chunk=chunk_002"));
    }

    #[test]
    fn markdown_report_includes_gap_section_even_when_empty() {
        let r = fixture_result();
        let md = render_markdown(&r);
        assert!(md.contains("Gap list"));
        assert!(md.contains("(none"));
    }

    #[test]
    fn provenance_summary_counts_y_layers() {
        let r = fixture_result();
        let s = ProvenanceSummary::from_result(&r);
        assert_eq!(s.passes_with_provenance, 1);
        assert_eq!(s.passes_without_provenance, 0);
        assert_eq!(s.triples_by_y_layer.get("y1"), Some(&1));
    }

    #[test]
    fn json_round_trips_through_serde() {
        let r = fixture_result();
        let json = serde_json::to_string(&r).unwrap();
        let parsed: ScenariosPassResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.per_cq.len(), 1);
        assert_eq!(parsed.per_cq[0].cq_id, "CQ-001");
        assert_eq!(parsed.per_cq[0].grade, Grade::Pass);
    }

    #[test]
    fn markdown_handles_persona_leak_grade_label() {
        let mut r = fixture_result();
        r.per_cq[0].grade = Grade::PersonaLeak;
        r.grade_counts = GradeCounts {
            persona_leak: 1,
            ..Default::default()
        };
        let md = render_markdown(&r);
        assert!(md.contains("persona_leak"));
    }

    #[test]
    fn markdown_round_trips_via_disk() {
        let r = fixture_result();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("report.md");
        write_markdown_report(&r, &p).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("Scenarios-pass report"));
    }

    #[test]
    fn json_round_trips_via_disk() {
        let r = fixture_result();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("report.json");
        write_json_report(&r, &p).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("\"being_label\""));
        assert!(body.contains("\"substantive_pass_rate\""));
    }
}
