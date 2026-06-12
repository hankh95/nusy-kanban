//! `nk regression` subcommands — VY-4313 EX-vi nightly regression baseline.
//!
//! Pairs two directories of `ScenariosPassResult` JSON files (a baseline
//! being's runs vs a candidate's) by `battery_label` and emits a regression
//! report. The nightly cron driver script (`scripts/nightly-regression.sh`)
//! invokes the actual EX-iv runs against V15 and V16, then calls `nk
//! regression compare` to produce the report.
//!
//! Exit code: 1 when any battery regressed beyond `--threshold-pp` (so
//! launchd surfaces the failure in its standard error log), 0 otherwise.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use nusy_evaluator::dashboard::load_results_from_dir;
use nusy_evaluator::regression::{
    DEFAULT_THRESHOLD_PP, RegressionReport, compare, render_markdown,
};

/// `nk regression` subcommands.
#[derive(Subcommand, Clone)]
pub enum RegressionCommands {
    /// Compare a baseline directory of result JSONs against a candidate
    /// directory and emit a regression report.
    Compare {
        /// Directory of baseline `ScenariosPassResult` JSON files.
        #[arg(long)]
        baseline_dir: PathBuf,
        /// Directory of candidate `ScenariosPassResult` JSON files.
        #[arg(long)]
        candidate_dir: PathBuf,
        /// Alert threshold in percentage points. Defaults to
        /// [`DEFAULT_THRESHOLD_PP`] (5.0). A regression of more than
        /// `threshold-pp` triggers an alert and a non-zero exit code.
        #[arg(long)]
        threshold_pp: Option<f64>,
        /// Output file. Format inferred from extension (`.json` or `.md`),
        /// or override with `--format`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Override the output format.
        #[arg(long, value_parser = ["json", "markdown", "md"])]
        format: Option<String>,
    },
}

/// Result of running a regression comparison.
#[derive(Debug)]
pub struct CompareOutcome {
    pub report: RegressionReport,
    pub written_to: Option<PathBuf>,
}

impl CompareOutcome {
    /// Whether the cron should exit non-zero. Mirrors
    /// `RegressionReport::has_regressions`.
    pub fn has_regressions(&self) -> bool {
        self.report.has_regressions()
    }
}

/// Top-level dispatch. Returns whether the comparison should fail the cron
/// (true → exit 1).
pub fn run(cmd: RegressionCommands) -> Result<bool, String> {
    match cmd {
        RegressionCommands::Compare {
            baseline_dir,
            candidate_dir,
            threshold_pp,
            out,
            format,
        } => {
            let outcome = compare_dirs(
                &baseline_dir,
                &candidate_dir,
                threshold_pp.unwrap_or(DEFAULT_THRESHOLD_PP),
                out.as_deref(),
                format.as_deref(),
            )?;
            print_summary(&outcome);
            Ok(outcome.has_regressions())
        }
    }
}

/// Run the comparison, optionally writing the report to disk.
pub fn compare_dirs(
    baseline_dir: &Path,
    candidate_dir: &Path,
    threshold_pp: f64,
    out: Option<&Path>,
    format_override: Option<&str>,
) -> Result<CompareOutcome, String> {
    let baseline = load_results_from_dir(baseline_dir).map_err(|e| e.to_string())?;
    let candidate = load_results_from_dir(candidate_dir).map_err(|e| e.to_string())?;
    let report = compare(&baseline, &candidate, threshold_pp);

    let written_to = if let Some(path) = out {
        let format = pick_format(path, format_override)?;
        let rendered = match format {
            ReportFormat::Json => serde_json::to_string_pretty(&report)
                .map_err(|e| format!("serialize regression report: {e}"))?,
            ReportFormat::Markdown => render_markdown(&report),
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|e| format!("create parent {}: {e}", parent.display()))?;
        }
        fs::write(path, &rendered).map_err(|e| format!("write {}: {e}", path.display()))?;
        Some(path.to_path_buf())
    } else {
        None
    };

    Ok(CompareOutcome { report, written_to })
}

#[derive(Debug, Clone, Copy)]
enum ReportFormat {
    Json,
    Markdown,
}

fn pick_format(path: &Path, override_: Option<&str>) -> Result<ReportFormat, String> {
    if let Some(fmt) = override_ {
        return match fmt {
            "json" => Ok(ReportFormat::Json),
            "markdown" | "md" => Ok(ReportFormat::Markdown),
            other => Err(format!("unknown format: {other}")),
        };
    }
    match path.extension().and_then(|s| s.to_str()) {
        Some("json") => Ok(ReportFormat::Json),
        Some("md") | Some("markdown") => Ok(ReportFormat::Markdown),
        Some(other) => Err(format!(
            "cannot infer format from extension '{other}'; pass --format json|markdown"
        )),
        None => Err("output path has no extension; pass --format json|markdown".to_string()),
    }
}

fn print_summary(outcome: &CompareOutcome) {
    let r = &outcome.report;
    println!(
        "Regression compare: paired={}, alerts={}, threshold=±{:.1}pp",
        r.paired_count,
        r.alerts.len(),
        r.threshold_pp,
    );
    for a in &r.alerts {
        println!(
            "  ALERT  {}: {:.1}% → {:.1}% (Δ {:+.1}pp)",
            a.battery_label,
            a.baseline_rate * 100.0,
            a.candidate_rate * 100.0,
            a.delta_pp,
        );
    }
    if let Some(path) = &outcome.written_to {
        println!("Report: {}", path.display());
    }
    if !r.unpaired_baseline.is_empty() {
        println!(
            "Unpaired baseline batteries (no candidate): {}",
            r.unpaired_baseline.join(", ")
        );
    }
    if !r.unpaired_candidate.is_empty() {
        println!(
            "Unpaired candidate batteries (no baseline): {}",
            r.unpaired_candidate.join(", ")
        );
    }
    if outcome.has_regressions() {
        eprintln!(
            "REGRESSION: {} batteries regressed beyond ±{:.1}pp",
            r.alerts.len(),
            r.threshold_pp,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_format_recognizes_extensions() {
        assert!(matches!(
            pick_format(Path::new("/tmp/x.json"), None).unwrap(),
            ReportFormat::Json
        ));
        assert!(matches!(
            pick_format(Path::new("/tmp/x.md"), None).unwrap(),
            ReportFormat::Markdown
        ));
        assert!(matches!(
            pick_format(Path::new("/tmp/x.markdown"), None).unwrap(),
            ReportFormat::Markdown
        ));
    }

    #[test]
    fn pick_format_rejects_unknown_extension() {
        let err = pick_format(Path::new("/tmp/x.yaml"), None).unwrap_err();
        assert!(err.contains("yaml"));
    }

    #[test]
    fn pick_format_override_wins_over_extension() {
        assert!(matches!(
            pick_format(Path::new("/tmp/x.txt"), Some("json")).unwrap(),
            ReportFormat::Json
        ));
        assert!(matches!(
            pick_format(Path::new("/tmp/x.json"), Some("markdown")).unwrap(),
            ReportFormat::Markdown
        ));
    }

    #[test]
    fn pick_format_extensionless_path_demands_override() {
        let err = pick_format(Path::new("/tmp/no_ext"), None).unwrap_err();
        assert!(err.contains("--format"));
    }

    #[test]
    fn missing_baseline_dir_returns_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cand = tmp.path().join("c");
        fs::create_dir(&cand).unwrap();
        let err = compare_dirs(
            Path::new("/nonexistent/baseline/from/ex4337"),
            &cand,
            5.0,
            None,
            None,
        )
        .expect_err("missing baseline must error");
        assert!(err.contains("/nonexistent/baseline/from/ex4337"));
    }
}
