//! EX-4336 / VY-4313 EX-v — Per-doc / per-level / per-being metrics dashboard.
//!
//! Consumes the `ScenariosPassResult` JSON files emitted by the EX-iv runner
//! ([`crate::test_scenarios`]) and renders four views the validation voyage
//! needs:
//!
//! 1. **Latest-run table** — per-doc / per-being snapshot of the most
//!    recent run, with substantive / liberal pass rates and runtime.
//! 2. **Per-being trend lines** — pass-rate over time per being, so V15→V16
//!    progressions are visible at a glance (Phase 3 of the chore body).
//! 3. **Per-CQ failure heatmap** — which CQs fail most often across all
//!    runs, surfaced for failure analysis (Phase 4).
//! 4. **Headline summary** — total runs, beings tested, batteries tested,
//!    median substantive pass rate.
//!
//! Output formats: HTML (the canonical artifact at
//! `research/shared/eval-data/vy-v16/metrics.html`) and markdown (for terminal
//! viewing / inclusion in PR bodies).
//!
//! ## Pipeline
//!
//! ```text
//! results-dir/*.json  (ScenariosPassResult per file)
//!   → load_results_from_dir
//!   → DashboardModel (grouped + sorted)
//!   → render_html / render_markdown
//!   → metrics.html / metrics.md
//! ```
//!
//! Renderers do not perform I/O — they take a `DashboardModel` and return a
//! `String`, so they are unit-testable without tempdirs.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::grader::Grade;
use crate::scenarios::ScenariosPassResult;

/// One row in the latest-run table — the most recent run for a
/// (being_label, battery_label) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestRun {
    pub being_label: String,
    pub battery_label: String,
    pub run_at: DateTime<Utc>,
    pub substantive_pass_rate: f64,
    pub substantive_passes: usize,
    pub substantive_eligible: usize,
    pub liberal_pass_rate: f64,
    pub liberal_passes: usize,
    pub total_cqs: usize,
    pub gap_count: usize,
    pub total_runtime_secs: Option<f64>,
}

/// One point on a per-being trend line. Multiple per being.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendPoint {
    pub run_at: DateTime<Utc>,
    pub battery_label: String,
    pub substantive_pass_rate: f64,
    pub liberal_pass_rate: f64,
}

/// Trend timeline for a single being (sorted by `run_at` ascending).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeingTrend {
    pub being_label: String,
    pub points: Vec<TrendPoint>,
}

/// Aggregate failure stats for one CQ id across all runs in the dashboard
/// window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CqFailureStats {
    pub cq_id: String,
    pub question: String,
    pub dimension: String,
    /// Beings + batteries that have ever asked this CQ.
    pub run_count: usize,
    pub fail_count: usize,
    pub refuse_count: usize,
    pub persona_leak_count: usize,
    pub error_count: usize,
    /// `(fail + persona_leak + error) / run_count` — failures that count
    /// against substantive pass rate.
    pub substantive_failure_rate: f64,
}

/// The full dashboard model — what renderers consume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardModel {
    pub generated_at: DateTime<Utc>,
    pub source_dir: String,
    pub total_runs: usize,
    pub latest_per_being_battery: Vec<LatestRun>,
    pub trends: Vec<BeingTrend>,
    pub cq_failures: Vec<CqFailureStats>,
}

/// Errors from dashboard ingestion.
#[derive(Debug, thiserror::Error)]
pub enum DashboardError {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Walk a directory non-recursively and load every `*.json` that
/// deserializes to a [`ScenariosPassResult`]. Files that fail to parse are
/// returned as errors so the caller can surface them; partial success is
/// not silent.
pub fn load_results_from_dir(dir: &Path) -> Result<Vec<ScenariosPassResult>, DashboardError> {
    let entries = fs::read_dir(dir).map_err(|e| DashboardError::Io {
        path: dir.display().to_string(),
        source: e,
    })?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path).map_err(|e| DashboardError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        let parsed: ScenariosPassResult =
            serde_json::from_slice(&bytes).map_err(|e| DashboardError::Parse {
                path: path.display().to_string(),
                source: e,
            })?;
        out.push(parsed);
    }
    Ok(out)
}

/// Build the dashboard model. Pure function: aggregations only.
pub fn build_model(results: &[ScenariosPassResult], source_dir: &str) -> DashboardModel {
    DashboardModel {
        generated_at: Utc::now(),
        source_dir: source_dir.to_string(),
        total_runs: results.len(),
        latest_per_being_battery: latest_per_being_battery(results),
        trends: per_being_trends(results),
        cq_failures: per_cq_failures(results),
    }
}

fn latest_per_being_battery(results: &[ScenariosPassResult]) -> Vec<LatestRun> {
    // Key by (being, battery) → most recent run.
    let mut latest: BTreeMap<(String, String), &ScenariosPassResult> = BTreeMap::new();
    for r in results {
        let key = (r.being_label.clone(), r.battery_label.clone());
        match latest.get(&key) {
            Some(existing) if existing.run_at >= r.run_at => {}
            _ => {
                latest.insert(key, r);
            }
        }
    }
    let mut rows: Vec<LatestRun> = latest
        .into_values()
        .map(|r| LatestRun {
            being_label: r.being_label.clone(),
            battery_label: r.battery_label.clone(),
            run_at: r.run_at,
            substantive_pass_rate: r.substantive_pass_rate,
            substantive_passes: r.substantive_passes,
            substantive_eligible: r.substantive_eligible,
            liberal_pass_rate: r.liberal_pass_rate,
            liberal_passes: r.liberal_passes,
            total_cqs: r.total_cqs,
            gap_count: r.gap_list.len(),
            total_runtime_secs: r.total_runtime_secs,
        })
        .collect();
    // Stable order: being then battery.
    rows.sort_by(|a, b| {
        a.being_label
            .cmp(&b.being_label)
            .then_with(|| a.battery_label.cmp(&b.battery_label))
    });
    rows
}

fn per_being_trends(results: &[ScenariosPassResult]) -> Vec<BeingTrend> {
    let mut by_being: BTreeMap<String, Vec<TrendPoint>> = BTreeMap::new();
    for r in results {
        by_being
            .entry(r.being_label.clone())
            .or_default()
            .push(TrendPoint {
                run_at: r.run_at,
                battery_label: r.battery_label.clone(),
                substantive_pass_rate: r.substantive_pass_rate,
                liberal_pass_rate: r.liberal_pass_rate,
            });
    }
    by_being
        .into_iter()
        .map(|(being_label, mut points)| {
            points.sort_by(|a, b| a.run_at.cmp(&b.run_at));
            BeingTrend {
                being_label,
                points,
            }
        })
        .collect()
}

fn per_cq_failures(results: &[ScenariosPassResult]) -> Vec<CqFailureStats> {
    // Aggregate by cq_id. Question + dimension take the latest seen value
    // (CQ specs may evolve across runs; show the most recent).
    let mut agg: BTreeMap<String, CqFailureAgg> = BTreeMap::new();
    for r in results {
        for pr in &r.per_cq {
            let entry = agg.entry(pr.cq_id.clone()).or_insert_with(|| CqFailureAgg {
                question: pr.question.clone(),
                dimension: pr.dimension.clone(),
                run_count: 0,
                fail: 0,
                refuse: 0,
                persona_leak: 0,
                error: 0,
            });
            entry.question = pr.question.clone();
            entry.dimension = pr.dimension.clone();
            entry.run_count += 1;
            match pr.grade {
                Grade::Pass => {}
                Grade::Fail => entry.fail += 1,
                Grade::Refuse => entry.refuse += 1,
                Grade::PersonaLeak => entry.persona_leak += 1,
                Grade::Error => entry.error += 1,
            }
        }
    }
    let mut stats: Vec<CqFailureStats> = agg
        .into_iter()
        .map(|(cq_id, a)| {
            let substantive_failures = a.fail + a.persona_leak + a.error;
            let rate = if a.run_count == 0 {
                0.0
            } else {
                substantive_failures as f64 / a.run_count as f64
            };
            CqFailureStats {
                cq_id,
                question: a.question,
                dimension: a.dimension,
                run_count: a.run_count,
                fail_count: a.fail,
                refuse_count: a.refuse,
                persona_leak_count: a.persona_leak,
                error_count: a.error,
                substantive_failure_rate: rate,
            }
        })
        .collect();
    // Worst offenders first; secondary sort by cq_id for stable ordering.
    stats.sort_by(|a, b| {
        b.substantive_failure_rate
            .partial_cmp(&a.substantive_failure_rate)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.cq_id.cmp(&b.cq_id))
    });
    stats
}

struct CqFailureAgg {
    question: String,
    dimension: String,
    run_count: usize,
    fail: usize,
    refuse: usize,
    persona_leak: usize,
    error: usize,
}

// ─── Renderers ──────────────────────────────────────────────────────────────

/// HTML render of the dashboard. Intentionally framework-free — a single
/// self-contained file with inline `<style>`. Tables only; no JS.
pub fn render_html(model: &DashboardModel) -> String {
    let mut out = String::new();
    out.push_str(&html_header(model));
    out.push_str(&html_summary(model));
    out.push_str(&html_latest_table(model));
    out.push_str(&html_trends(model));
    out.push_str(&html_cq_failures(model));
    out.push_str("</body>\n</html>\n");
    out
}

fn html_header(model: &DashboardModel) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VY-4313 metrics dashboard</title>
<style>
body {{ font-family: -apple-system, system-ui, sans-serif; max-width: 1100px; margin: 2em auto; padding: 0 1em; color: #222; }}
h1 {{ border-bottom: 2px solid #444; padding-bottom: 0.3em; }}
h2 {{ margin-top: 2em; border-bottom: 1px solid #aaa; padding-bottom: 0.2em; }}
table {{ border-collapse: collapse; width: 100%; margin: 1em 0; }}
th, td {{ border: 1px solid #ccc; padding: 0.4em 0.7em; text-align: left; vertical-align: top; }}
th {{ background: #eee; }}
tr:nth-child(even) {{ background: #f9f9f9; }}
.pass-good {{ color: #0a5; font-weight: bold; }}
.pass-warn {{ color: #b60; font-weight: bold; }}
.pass-bad  {{ color: #c33; font-weight: bold; }}
.muted {{ color: #888; }}
.metric {{ font-variant-numeric: tabular-nums; }}
.cq-question {{ max-width: 400px; }}
.footer {{ color: #888; font-size: 0.85em; margin-top: 3em; border-top: 1px solid #ddd; padding-top: 0.5em; }}
</style>
</head>
<body>
<h1>VY-4313 metrics dashboard</h1>
<p class="muted">Generated {} from <code>{}</code> ({} runs ingested).</p>
"#,
        model.generated_at.to_rfc3339(),
        html_escape(&model.source_dir),
        model.total_runs,
    )
}

fn html_summary(model: &DashboardModel) -> String {
    let beings: std::collections::BTreeSet<_> = model
        .latest_per_being_battery
        .iter()
        .map(|r| r.being_label.as_str())
        .collect();
    let batteries: std::collections::BTreeSet<_> = model
        .latest_per_being_battery
        .iter()
        .map(|r| r.battery_label.as_str())
        .collect();
    let median = median_substantive_pass_rate(&model.latest_per_being_battery);
    format!(
        r#"<h2>Headline</h2>
<table>
<tr><th>Total runs</th><td class="metric">{}</td></tr>
<tr><th>Beings tested</th><td class="metric">{}</td></tr>
<tr><th>Batteries tested</th><td class="metric">{}</td></tr>
<tr><th>Median substantive pass rate (latest per being×battery)</th><td class="metric">{}</td></tr>
</table>
"#,
        model.total_runs,
        beings.len(),
        batteries.len(),
        format_pct_html(median),
    )
}

fn html_latest_table(model: &DashboardModel) -> String {
    if model.latest_per_being_battery.is_empty() {
        return "<h2>Latest run per being × battery</h2>\n<p class=\"muted\">No runs yet.</p>\n"
            .to_string();
    }
    let mut out = String::from(
        "<h2>Latest run per being × battery</h2>\n<table>\n<tr><th>Being</th><th>Battery</th><th>Run at</th><th>Substantive</th><th>Liberal</th><th>Gaps</th><th>Runtime</th></tr>\n",
    );
    for r in &model.latest_per_being_battery {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"metric\">{} ({} / {})</td><td class=\"metric\">{} ({} / {})</td><td class=\"metric\">{}</td><td class=\"metric\">{}</td></tr>\n",
            html_escape(&r.being_label),
            html_escape(&r.battery_label),
            r.run_at.to_rfc3339(),
            format_pct_html(r.substantive_pass_rate),
            r.substantive_passes,
            r.substantive_eligible,
            format_pct_html(r.liberal_pass_rate),
            r.liberal_passes,
            r.total_cqs,
            r.gap_count,
            format_runtime_html(r.total_runtime_secs),
        ));
    }
    out.push_str("</table>\n");
    out
}

fn html_trends(model: &DashboardModel) -> String {
    if model.trends.is_empty() {
        return String::new();
    }
    let mut out = String::from("<h2>Per-being trend (chronological)</h2>\n");
    for trend in &model.trends {
        out.push_str(&format!(
            "<h3>{}</h3>\n<table>\n<tr><th>Run at</th><th>Battery</th><th>Substantive</th><th>Liberal</th><th>Δ vs prev</th></tr>\n",
            html_escape(&trend.being_label),
        ));
        let mut prev: Option<f64> = None;
        for p in &trend.points {
            let delta = match prev {
                Some(prev_rate) => format_delta_html(p.substantive_pass_rate - prev_rate),
                None => "<span class=\"muted\">—</span>".to_string(),
            };
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td class=\"metric\">{}</td><td class=\"metric\">{}</td><td class=\"metric\">{}</td></tr>\n",
                p.run_at.to_rfc3339(),
                html_escape(&p.battery_label),
                format_pct_html(p.substantive_pass_rate),
                format_pct_html(p.liberal_pass_rate),
                delta,
            ));
            prev = Some(p.substantive_pass_rate);
        }
        out.push_str("</table>\n");
    }
    out
}

fn html_cq_failures(model: &DashboardModel) -> String {
    if model.cq_failures.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "<h2>Per-CQ failure breakdown (worst offenders first)</h2>\n<p class=\"muted\">Substantive failure rate counts <code>fail + persona_leak + error</code>; refusals shown separately.</p>\n<table>\n<tr><th>CQ</th><th>Dimension</th><th>Question</th><th>Runs</th><th>Fail</th><th>Refuse</th><th>Leak</th><th>Error</th><th>Sub-failure rate</th></tr>\n",
    );
    for s in &model.cq_failures {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td class=\"cq-question\">{}</td><td class=\"metric\">{}</td><td class=\"metric\">{}</td><td class=\"metric\">{}</td><td class=\"metric\">{}</td><td class=\"metric\">{}</td><td class=\"metric\">{}</td></tr>\n",
            html_escape(&s.cq_id),
            html_escape(&s.dimension),
            html_escape(&s.question),
            s.run_count,
            s.fail_count,
            s.refuse_count,
            s.persona_leak_count,
            s.error_count,
            format_pct_html(s.substantive_failure_rate),
        ));
    }
    out.push_str("</table>\n<p class=\"footer\">EX-4336 / VY-4313 EX-v.</p>\n");
    out
}

fn format_pct_html(rate: f64) -> String {
    let pct = (rate * 100.0).clamp(0.0, 100.0);
    let class = if pct >= 65.0 {
        "pass-good"
    } else if pct >= 40.0 {
        "pass-warn"
    } else {
        "pass-bad"
    };
    format!("<span class=\"{class}\">{pct:.1}%</span>")
}

fn format_delta_html(delta: f64) -> String {
    let pp = delta * 100.0;
    let class = if pp > 0.5 {
        "pass-good"
    } else if pp < -0.5 {
        "pass-bad"
    } else {
        "muted"
    };
    let sign = if pp > 0.0 { "+" } else { "" };
    format!("<span class=\"{class}\">{sign}{pp:.1}pp</span>")
}

fn format_runtime_html(secs: Option<f64>) -> String {
    match secs {
        Some(s) => format!("{s:.1}s"),
        None => "<span class=\"muted\">—</span>".to_string(),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn median_substantive_pass_rate(rows: &[LatestRun]) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    let mut rates: Vec<f64> = rows.iter().map(|r| r.substantive_pass_rate).collect();
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let mid = rates.len() / 2;
    if rates.len().is_multiple_of(2) {
        (rates[mid - 1] + rates[mid]) / 2.0
    } else {
        rates[mid]
    }
}

/// Markdown render of the dashboard. Mirrors the HTML structure for
/// terminal viewing and PR-body inclusion.
pub fn render_markdown(model: &DashboardModel) -> String {
    let mut out = String::new();
    out.push_str("# VY-4313 metrics dashboard\n\n");
    out.push_str(&format!(
        "Generated {} from `{}` ({} runs ingested).\n\n",
        model.generated_at.to_rfc3339(),
        model.source_dir,
        model.total_runs,
    ));

    let beings: std::collections::BTreeSet<_> = model
        .latest_per_being_battery
        .iter()
        .map(|r| r.being_label.as_str())
        .collect();
    let batteries: std::collections::BTreeSet<_> = model
        .latest_per_being_battery
        .iter()
        .map(|r| r.battery_label.as_str())
        .collect();
    let median = median_substantive_pass_rate(&model.latest_per_being_battery);

    out.push_str("## Headline\n\n");
    out.push_str(&format!("- Total runs: **{}**\n", model.total_runs));
    out.push_str(&format!("- Beings tested: **{}**\n", beings.len()));
    out.push_str(&format!("- Batteries tested: **{}**\n", batteries.len()));
    out.push_str(&format!(
        "- Median substantive pass rate: **{:.1}%**\n\n",
        median * 100.0,
    ));

    out.push_str("## Latest run per being × battery\n\n");
    if model.latest_per_being_battery.is_empty() {
        out.push_str("_No runs yet._\n\n");
    } else {
        out.push_str(
            "| Being | Battery | Run at | Substantive | Liberal | Gaps | Runtime |\n|---|---|---|---|---|---|---|\n",
        );
        for r in &model.latest_per_being_battery {
            out.push_str(&format!(
                "| {} | {} | {} | {:.1}% ({}/{}) | {:.1}% ({}/{}) | {} | {} |\n",
                r.being_label,
                r.battery_label,
                r.run_at.to_rfc3339(),
                r.substantive_pass_rate * 100.0,
                r.substantive_passes,
                r.substantive_eligible,
                r.liberal_pass_rate * 100.0,
                r.liberal_passes,
                r.total_cqs,
                r.gap_count,
                r.total_runtime_secs
                    .map(|s| format!("{s:.1}s"))
                    .unwrap_or_else(|| "—".to_string()),
            ));
        }
        out.push('\n');
    }

    if !model.trends.is_empty() {
        out.push_str("## Per-being trend (chronological)\n\n");
        for trend in &model.trends {
            out.push_str(&format!("### {}\n\n", trend.being_label));
            out.push_str(
                "| Run at | Battery | Substantive | Liberal | Δ vs prev |\n|---|---|---|---|---|\n",
            );
            let mut prev: Option<f64> = None;
            for p in &trend.points {
                let delta = match prev {
                    Some(prev_rate) => {
                        let pp = (p.substantive_pass_rate - prev_rate) * 100.0;
                        let sign = if pp > 0.0 { "+" } else { "" };
                        format!("{sign}{pp:.1}pp")
                    }
                    None => "—".to_string(),
                };
                out.push_str(&format!(
                    "| {} | {} | {:.1}% | {:.1}% | {} |\n",
                    p.run_at.to_rfc3339(),
                    p.battery_label,
                    p.substantive_pass_rate * 100.0,
                    p.liberal_pass_rate * 100.0,
                    delta,
                ));
                prev = Some(p.substantive_pass_rate);
            }
            out.push('\n');
        }
    }

    if !model.cq_failures.is_empty() {
        out.push_str("## Per-CQ failure breakdown (worst offenders first)\n\n");
        out.push_str(
            "| CQ | Dimension | Question | Runs | Fail | Refuse | Leak | Error | Sub-failure rate |\n|---|---|---|---|---|---|---|---|---|\n",
        );
        for s in &model.cq_failures {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {:.1}% |\n",
                s.cq_id,
                s.dimension,
                s.question.replace('|', "\\|"),
                s.run_count,
                s.fail_count,
                s.refuse_count,
                s.persona_leak_count,
                s.error_count,
                s.substantive_failure_rate * 100.0,
            ));
        }
        out.push('\n');
    }

    out.push_str("---\n\nEX-4336 / VY-4313 EX-v.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::{Battery, CqSpec, Expect};
    use crate::grader::{Grader, GraderConfig};
    use crate::scenarios::{BeingResponse, test_scenarios};
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

    fn run(
        being: &str,
        battery_label: &str,
        cqs: Vec<(&str, &str, &str)>,
        responder: impl Fn(&CqSpec, &str) -> BeingResponse,
        run_at: DateTime<Utc>,
    ) -> ScenariosPassResult {
        let battery = Battery {
            cqs: cqs
                .into_iter()
                .map(|(id, q, dim)| CqSpec {
                    id: id.into(),
                    question: q.into(),
                    expected_keywords: vec![],
                    expected_resolution: Vec::new(),
                    expect: Expect::Answer,
                    dimension: dim.into(),
                    domain: "general_education".into(),
                    requirement_id: None,
                    scenario_id: None,
                    tutor_seal_hash: None,
                })
                .collect(),
            source_label: battery_label.to_string(),
        };
        let grader = Grader::new(GraderConfig {
            require_graph_trace: false,
        });
        let mut result = test_scenarios(being, &battery, &grader, responder);
        // Override run_at so trend ordering is deterministic in tests.
        result.run_at = run_at;
        result
    }

    fn always_fail(_cq: &CqSpec, _q: &str) -> BeingResponse {
        BeingResponse {
            response: "I'm not sure.".to_string(),
            trail: empty_trail(),
        }
    }

    fn always_pass_with_keywords(_cq: &CqSpec, _q: &str) -> BeingResponse {
        BeingResponse {
            response: "An archer uses a bow to shoot.".to_string(),
            trail: empty_trail(),
        }
    }

    #[test]
    fn build_model_groups_latest_per_being_battery() {
        let early = "2026-05-01T00:00:00Z".parse().unwrap();
        let late = "2026-05-05T00:00:00Z".parse().unwrap();
        let r1 = run(
            "santiago-toddler-v15",
            "alphabet",
            vec![("CQ-1", "What is an archer?", "word_meaning")],
            always_fail,
            early,
        );
        let r2 = run(
            "santiago-toddler-v15",
            "alphabet",
            vec![("CQ-1", "What is an archer?", "word_meaning")],
            always_fail,
            late,
        );
        let model = build_model(&[r1, r2], "test");
        assert_eq!(model.total_runs, 2);
        assert_eq!(
            model.latest_per_being_battery.len(),
            1,
            "two runs of same being+battery dedupe to one latest row"
        );
        assert_eq!(model.latest_per_being_battery[0].run_at, late);
    }

    #[test]
    fn trends_are_sorted_chronologically_per_being() {
        let t1 = "2026-05-01T00:00:00Z".parse().unwrap();
        let t2 = "2026-05-03T00:00:00Z".parse().unwrap();
        let t3 = "2026-05-05T00:00:00Z".parse().unwrap();
        // Insert out of order.
        let r2 = run(
            "santiago-toddler-v16",
            "b",
            vec![("CQ-1", "q", "d")],
            always_fail,
            t2,
        );
        let r1 = run(
            "santiago-toddler-v16",
            "b",
            vec![("CQ-1", "q", "d")],
            always_fail,
            t1,
        );
        let r3 = run(
            "santiago-toddler-v16",
            "b",
            vec![("CQ-1", "q", "d")],
            always_fail,
            t3,
        );
        let model = build_model(&[r2, r1, r3], "test");
        assert_eq!(model.trends.len(), 1);
        let pts = &model.trends[0].points;
        assert_eq!(pts.len(), 3);
        assert!(pts[0].run_at < pts[1].run_at);
        assert!(pts[1].run_at < pts[2].run_at);
    }

    #[test]
    fn cq_failures_rank_worst_offenders_first() {
        // CQ-1 always fails (3/3), CQ-2 always passes (0/3 fails).
        let cqs = vec![
            ("CQ-1", "Which uses a bow?", "word_meaning"),
            ("CQ-2", "Which uses a bow?", "word_meaning"),
        ];
        let when =
            |i: i64| -> DateTime<Utc> { format!("2026-05-0{}T00:00:00Z", i).parse().unwrap() };
        let mixed = |cq: &CqSpec, _q: &str| BeingResponse {
            response: if cq.id == "CQ-1" {
                "no idea".into()
            } else {
                "An archer uses a bow.".into()
            },
            trail: empty_trail(),
        };
        let r1 = run("being", "battery", cqs.clone(), mixed, when(1));
        let r2 = run("being", "battery", cqs.clone(), mixed, when(2));
        let r3 = run("being", "battery", cqs, mixed, when(3));
        let model = build_model(&[r1, r2, r3], "test");
        // CQ-1 should be ranked first (highest sub-failure rate).
        assert_eq!(model.cq_failures.first().unwrap().cq_id, "CQ-1");
        // Substantive failure rate for CQ-1: fail=3, run_count=3 → 1.0.
        assert!((model.cq_failures[0].substantive_failure_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn render_html_contains_key_sections() {
        let r1 = run(
            "santiago-toddler-v16",
            "alphabet",
            vec![("CQ-1", "What is an archer?", "word_meaning")],
            always_pass_with_keywords,
            "2026-05-05T00:00:00Z".parse().unwrap(),
        );
        let model = build_model(&[r1], "test-dir");
        let html = render_html(&model);
        assert!(html.contains("<title>VY-4313 metrics dashboard</title>"));
        assert!(html.contains("Headline"));
        assert!(html.contains("Latest run per being"));
        assert!(html.contains("Per-being trend"));
        assert!(html.contains("santiago-toddler-v16"));
        assert!(html.contains("alphabet"));
    }

    #[test]
    fn render_html_escapes_special_chars() {
        let mut r = run(
            "<script>alert('x')</script>",
            "battery & friends",
            vec![("CQ-1", "Quote\"and<tag>", "word_meaning")],
            always_fail,
            "2026-05-05T00:00:00Z".parse().unwrap(),
        );
        // Source dir also goes through escape.
        let model = build_model(std::slice::from_ref(&r), "/path/with/<dangerous>&chars");
        let html = render_html(&model);
        assert!(!html.contains("<script>alert("));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("battery &amp; friends"));
        assert!(html.contains("&lt;dangerous&gt;&amp;chars"));
        // Suppress unused warning when test runs cleanly.
        r.being_label.clear();
    }

    #[test]
    fn render_markdown_emits_expected_sections() {
        let r1 = run(
            "santiago-toddler-v16",
            "alphabet",
            vec![("CQ-1", "What is an archer?", "word_meaning")],
            always_pass_with_keywords,
            "2026-05-05T00:00:00Z".parse().unwrap(),
        );
        let model = build_model(&[r1], "test");
        let md = render_markdown(&model);
        assert!(md.contains("# VY-4313 metrics dashboard"));
        assert!(md.contains("## Headline"));
        assert!(md.contains("## Latest run per being"));
        assert!(md.contains("santiago-toddler-v16"));
    }

    #[test]
    fn empty_results_render_does_not_panic_and_says_so() {
        let model = build_model(&[], "empty-dir");
        let html = render_html(&model);
        assert!(html.contains("No runs yet"));
        let md = render_markdown(&model);
        assert!(md.contains("_No runs yet._"));
    }

    #[test]
    fn pass_rate_thresholds_color_correctly() {
        // 70% → good (>= 65%)
        assert!(format_pct_html(0.70).contains("pass-good"));
        // 50% → warn (40 <= x < 65)
        assert!(format_pct_html(0.50).contains("pass-warn"));
        // 20% → bad
        assert!(format_pct_html(0.20).contains("pass-bad"));
    }

    #[test]
    fn delta_signs_render_correctly() {
        assert!(format_delta_html(0.10).contains("+10.0pp"));
        assert!(format_delta_html(0.10).contains("pass-good"));
        assert!(format_delta_html(-0.05).contains("-5.0pp"));
        assert!(format_delta_html(-0.05).contains("pass-bad"));
        assert!(format_delta_html(0.0).contains("muted"));
    }
}
