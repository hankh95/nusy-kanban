//! EX-4337 / VY-4313 EX-vi — Regression baseline comparison.
//!
//! Pairs two sets of `ScenariosPassResult` outputs (typically a baseline
//! being + a candidate being run on the same plates) and emits a regression
//! report: per-battery deltas, alerts on regressions beyond a threshold,
//! and an "any regression" predicate the CLI uses to set non-zero exit code
//! for the nightly cron.
//!
//! Pairing is by `battery_label`. For each side, only the most recent run
//! per battery is considered (so re-runs naturally overwrite). Beings that
//! ran on a battery the other side didn't are surfaced via
//! `unpaired_baseline` / `unpaired_candidate`.
//!
//! ## Pipeline
//!
//! ```text
//! baseline_dir/*.json    candidate_dir/*.json
//!         │                       │
//!         ▼                       ▼
//!   load_results_from_dir (from dashboard.rs)
//!         │                       │
//!         └────────► compare ◄────┘
//!                       │
//!                       ▼
//!              RegressionReport
//!                       │
//!         ┌─────────────┼─────────────┐
//!         ▼                           ▼
//!   render_markdown              has_regressions?
//!   render_json                  → exit 0 / 1
//! ```

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::scenarios::ScenariosPassResult;

/// Default alert threshold in percentage points. Chosen to match the
/// EX-κ acceptance bar (lift ≥14 pp) — a regression of more than 5pp on
/// any battery is loud enough to wake oncall.
pub const DEFAULT_THRESHOLD_PP: f64 = 5.0;

/// One battery worth of comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionPair {
    pub battery_label: String,
    pub baseline_being: String,
    pub baseline_rate: f64,
    pub baseline_run_at: DateTime<Utc>,
    pub candidate_being: String,
    pub candidate_rate: f64,
    pub candidate_run_at: DateTime<Utc>,
    /// `(candidate_rate - baseline_rate) * 100` — positive = improvement.
    pub delta_pp: f64,
    /// `delta_pp < -threshold_pp`.
    pub is_regression: bool,
}

/// One alert per regressed battery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionAlert {
    pub battery_label: String,
    pub baseline_rate: f64,
    pub candidate_rate: f64,
    pub delta_pp: f64,
}

/// Full comparison report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionReport {
    pub generated_at: DateTime<Utc>,
    pub baseline_label: String,
    pub candidate_label: String,
    pub threshold_pp: f64,
    pub pairs: Vec<RegressionPair>,
    pub alerts: Vec<RegressionAlert>,
    pub total_baseline_runs: usize,
    pub total_candidate_runs: usize,
    pub paired_count: usize,
    /// Battery labels seen only on the baseline side — candidate didn't run
    /// these (or its results are missing). The cron treats this as a soft
    /// warning; a fully-paired comparison is the goal.
    pub unpaired_baseline: Vec<String>,
    pub unpaired_candidate: Vec<String>,
}

impl RegressionReport {
    /// Whether any battery regressed beyond the configured threshold. The
    /// nightly CLI exits non-zero if this is true so launchd surfaces it.
    pub fn has_regressions(&self) -> bool {
        !self.alerts.is_empty()
    }
}

/// Pair the baseline and candidate result sets by `battery_label` and
/// compute deltas + alerts.
pub fn compare(
    baseline: &[ScenariosPassResult],
    candidate: &[ScenariosPassResult],
    threshold_pp: f64,
) -> RegressionReport {
    let base_map = latest_by_battery(baseline);
    let cand_map = latest_by_battery(candidate);

    let baseline_label = baseline
        .first()
        .map(|r| r.being_label.clone())
        .unwrap_or_default();
    let candidate_label = candidate
        .first()
        .map(|r| r.being_label.clone())
        .unwrap_or_default();

    let mut pairs = Vec::new();
    let mut alerts = Vec::new();
    let mut unpaired_baseline = Vec::new();
    let mut unpaired_candidate = Vec::new();

    let all_batteries: std::collections::BTreeSet<&str> = base_map
        .keys()
        .chain(cand_map.keys())
        .map(|s| s.as_str())
        .collect();

    for battery in all_batteries {
        match (base_map.get(battery), cand_map.get(battery)) {
            (Some(b), Some(c)) => {
                let delta_pp = (c.substantive_pass_rate - b.substantive_pass_rate) * 100.0;
                let is_regression = delta_pp < -threshold_pp;
                pairs.push(RegressionPair {
                    battery_label: battery.to_string(),
                    baseline_being: b.being_label.clone(),
                    baseline_rate: b.substantive_pass_rate,
                    baseline_run_at: b.run_at,
                    candidate_being: c.being_label.clone(),
                    candidate_rate: c.substantive_pass_rate,
                    candidate_run_at: c.run_at,
                    delta_pp,
                    is_regression,
                });
                if is_regression {
                    alerts.push(RegressionAlert {
                        battery_label: battery.to_string(),
                        baseline_rate: b.substantive_pass_rate,
                        candidate_rate: c.substantive_pass_rate,
                        delta_pp,
                    });
                }
            }
            (Some(_), None) => unpaired_baseline.push(battery.to_string()),
            (None, Some(_)) => unpaired_candidate.push(battery.to_string()),
            (None, None) => unreachable!(
                "battery seen in neither map but key came from union — bug in compare()"
            ),
        }
    }

    let paired_count = pairs.len();

    RegressionReport {
        generated_at: Utc::now(),
        baseline_label,
        candidate_label,
        threshold_pp,
        pairs,
        alerts,
        total_baseline_runs: baseline.len(),
        total_candidate_runs: candidate.len(),
        paired_count,
        unpaired_baseline,
        unpaired_candidate,
    }
}

fn latest_by_battery(results: &[ScenariosPassResult]) -> BTreeMap<String, &ScenariosPassResult> {
    let mut map: BTreeMap<String, &ScenariosPassResult> = BTreeMap::new();
    for r in results {
        match map.get(&r.battery_label) {
            Some(existing) if existing.run_at >= r.run_at => {}
            _ => {
                map.insert(r.battery_label.clone(), r);
            }
        }
    }
    map
}

/// Markdown render of the report — the format the nightly cron stashes
/// alongside the JSON for human triage.
pub fn render_markdown(report: &RegressionReport) -> String {
    let mut out = String::new();
    out.push_str("# Regression baseline report\n\n");
    out.push_str(&format!(
        "Generated **{}** — baseline `{}` vs candidate `{}` (threshold ±{:.1}pp).\n\n",
        report.generated_at.to_rfc3339(),
        report.baseline_label,
        report.candidate_label,
        report.threshold_pp,
    ));

    out.push_str("## Headline\n\n");
    out.push_str(&format!(
        "- Paired batteries: **{}**\n",
        report.paired_count
    ));
    out.push_str(&format!(
        "- Unpaired (baseline only): **{}**\n",
        report.unpaired_baseline.len()
    ));
    out.push_str(&format!(
        "- Unpaired (candidate only): **{}**\n",
        report.unpaired_candidate.len()
    ));
    out.push_str(&format!(
        "- Regressions (Δ < -{:.1}pp): **{}**\n\n",
        report.threshold_pp,
        report.alerts.len()
    ));

    if !report.alerts.is_empty() {
        out.push_str("## Alerts\n\n");
        out.push_str("| Battery | Baseline | Candidate | Δ pp |\n|---|---|---|---|\n");
        for a in &report.alerts {
            out.push_str(&format!(
                "| {} | {:.1}% | {:.1}% | {:+.1} |\n",
                a.battery_label,
                a.baseline_rate * 100.0,
                a.candidate_rate * 100.0,
                a.delta_pp,
            ));
        }
        out.push('\n');
    }

    if !report.pairs.is_empty() {
        out.push_str("## All paired batteries\n\n");
        out.push_str("| Battery | Baseline run | Candidate run | Baseline | Candidate | Δ pp | Regression |\n|---|---|---|---|---|---|---|\n");
        for p in &report.pairs {
            out.push_str(&format!(
                "| {} | {} | {} | {:.1}% | {:.1}% | {:+.1} | {} |\n",
                p.battery_label,
                p.baseline_run_at.to_rfc3339(),
                p.candidate_run_at.to_rfc3339(),
                p.baseline_rate * 100.0,
                p.candidate_rate * 100.0,
                p.delta_pp,
                if p.is_regression { "✗" } else { "✓" },
            ));
        }
        out.push('\n');
    }

    if !report.unpaired_baseline.is_empty() {
        out.push_str("## Unpaired (baseline only)\n\n");
        for b in &report.unpaired_baseline {
            out.push_str(&format!("- {b}\n"));
        }
        out.push('\n');
    }
    if !report.unpaired_candidate.is_empty() {
        out.push_str("## Unpaired (candidate only)\n\n");
        for b in &report.unpaired_candidate {
            out.push_str(&format!("- {b}\n"));
        }
        out.push('\n');
    }

    out.push_str("---\n\nEX-4337 / VY-4313 EX-vi.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grader::Grade;
    use crate::provenance::PassResult;
    use crate::scenarios::GradeCounts;

    fn pr(cq_id: &str, grade: Grade) -> PassResult {
        PassResult {
            cq_id: cq_id.to_string(),
            question: "q".to_string(),
            dimension: "word_meaning".to_string(),
            grade,
            response: "r".to_string(),
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

    fn at(day: u32) -> DateTime<Utc> {
        format!("2026-05-0{day}T00:00:00Z").parse().unwrap()
    }

    #[test]
    fn pairing_matches_by_battery_label_and_takes_latest_per_side() {
        // Two runs on the same battery on each side; compare must use the
        // latest one.
        let baseline = vec![
            synth("v15", "alphabet", at(1), 1, 4), // 20% — old, should be ignored
            synth("v15", "alphabet", at(3), 4, 1), // 80% — latest
        ];
        let candidate = vec![synth("v16", "alphabet", at(2), 3, 2)]; // 60%
        let report = compare(&baseline, &candidate, DEFAULT_THRESHOLD_PP);
        assert_eq!(report.paired_count, 1);
        assert!((report.pairs[0].baseline_rate - 0.80).abs() < 1e-9);
        assert!((report.pairs[0].candidate_rate - 0.60).abs() < 1e-9);
        assert!((report.pairs[0].delta_pp - (-20.0)).abs() < 1e-9);
        assert!(report.pairs[0].is_regression);
    }

    #[test]
    fn alerts_only_fire_below_negative_threshold() {
        // Improvement, neutral, mild dip (under threshold), big dip (over).
        let baseline = vec![
            synth("v15", "improvement", at(1), 5, 5), // 50%
            synth("v15", "neutral", at(1), 5, 5),     // 50%
            synth("v15", "mild_dip", at(1), 5, 5),    // 50%
            synth("v15", "big_dip", at(1), 5, 5),     // 50%
        ];
        let candidate = vec![
            synth("v16", "improvement", at(2), 8, 2), // 80% (+30pp)
            synth("v16", "neutral", at(2), 5, 5),     // 50% (0pp)
            synth("v16", "mild_dip", at(2), 9, 11),   // 45% (-5pp; right at threshold)
            synth("v16", "big_dip", at(2), 2, 8),     // 20% (-30pp)
        ];
        let report = compare(&baseline, &candidate, 5.0);
        assert_eq!(report.alerts.len(), 1, "only big_dip should alert");
        assert_eq!(report.alerts[0].battery_label, "big_dip");
        // mild_dip is right at the threshold — not strictly below, so no alert.
        assert!(report.has_regressions());
    }

    #[test]
    fn all_improvements_means_no_regressions_no_alerts() {
        let baseline = vec![synth("v15", "alphabet", at(1), 1, 9)]; // 10%
        let candidate = vec![synth("v16", "alphabet", at(2), 9, 1)]; // 90%
        let report = compare(&baseline, &candidate, DEFAULT_THRESHOLD_PP);
        assert!(!report.has_regressions());
        assert_eq!(report.alerts.len(), 0);
        assert!(report.pairs[0].delta_pp > 0.0);
    }

    #[test]
    fn unpaired_batteries_are_surfaced_separately() {
        let baseline = vec![
            synth("v15", "shared", at(1), 5, 5),
            synth("v15", "only_baseline", at(1), 5, 5),
        ];
        let candidate = vec![
            synth("v16", "shared", at(1), 5, 5),
            synth("v16", "only_candidate", at(1), 5, 5),
        ];
        let report = compare(&baseline, &candidate, DEFAULT_THRESHOLD_PP);
        assert_eq!(report.paired_count, 1);
        assert_eq!(report.unpaired_baseline, vec!["only_baseline"]);
        assert_eq!(report.unpaired_candidate, vec!["only_candidate"]);
    }

    #[test]
    fn empty_inputs_produce_empty_report_no_panic() {
        let report = compare(&[], &[], DEFAULT_THRESHOLD_PP);
        assert_eq!(report.paired_count, 0);
        assert!(!report.has_regressions());
        assert_eq!(report.baseline_label, "");
        assert_eq!(report.candidate_label, "");
    }

    #[test]
    fn render_markdown_lists_alerts_section_when_regressions_present() {
        let baseline = vec![synth("v15-toddler", "alphabet", at(1), 5, 5)];
        let candidate = vec![synth("v16-toddler", "alphabet", at(2), 1, 9)]; // -40pp
        let report = compare(&baseline, &candidate, 5.0);
        let md = render_markdown(&report);
        assert!(md.contains("# Regression baseline report"));
        assert!(md.contains("v15-toddler"));
        assert!(md.contains("v16-toddler"));
        assert!(md.contains("## Alerts"));
        assert!(md.contains("alphabet"));
        assert!(md.contains("-40.0"));
    }

    #[test]
    fn render_markdown_omits_alerts_section_when_clean() {
        let baseline = vec![synth("v15", "alphabet", at(1), 5, 5)];
        let candidate = vec![synth("v16", "alphabet", at(2), 7, 3)]; // +20pp
        let report = compare(&baseline, &candidate, 5.0);
        let md = render_markdown(&report);
        assert!(!md.contains("## Alerts"));
    }

    #[test]
    fn delta_sign_convention_candidate_minus_baseline() {
        // Candidate at 70%, baseline at 50% → +20pp.
        let baseline = vec![synth("v15", "alphabet", at(1), 5, 5)];
        let candidate = vec![synth("v16", "alphabet", at(2), 7, 3)];
        let report = compare(&baseline, &candidate, 5.0);
        assert!((report.pairs[0].delta_pp - 20.0).abs() < 1e-9);
    }

    #[test]
    fn report_round_trips_through_serde() {
        let baseline = vec![synth("v15", "alphabet", at(1), 5, 5)];
        let candidate = vec![synth("v16", "alphabet", at(2), 1, 9)];
        let report = compare(&baseline, &candidate, 5.0);
        let json = serde_json::to_string(&report).expect("serialize");
        let _back: RegressionReport = serde_json::from_str(&json).expect("deserialize");
    }
}
