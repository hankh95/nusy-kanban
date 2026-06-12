//! `nk dashboard` subcommands — VY-4313 metrics dashboard surface.
//!
//! Thin CLI wrapper around `nusy_evaluator::dashboard`. Reads the
//! `ScenariosPassResult` JSON files emitted by the EX-iv runner from a
//! results directory and renders the per-doc / per-level / per-being
//! dashboard at the canonical artifact path
//! (`research/shared/eval-data/vy-v16/metrics.html`) or wherever the user
//! points `--out`.

use std::fs;
use std::path::PathBuf;

use clap::Subcommand;
use nusy_evaluator::dashboard::{build_model, load_results_from_dir, render_html, render_markdown};

/// `nk dashboard` subcommands.
#[derive(Subcommand, Clone)]
pub enum DashboardCommands {
    /// Ingest a directory of `ScenariosPassResult` JSON files and render
    /// the metrics dashboard.
    Render {
        /// Directory containing `*.json` result files (one per run).
        #[arg(long)]
        results_dir: PathBuf,
        /// Output file. Defaults to
        /// `research/shared/eval-data/vy-v16/metrics.html` for the html
        /// format and `…/metrics.md` for markdown.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Rendering format.
        #[arg(long, value_parser = ["html", "markdown", "md"], default_value = "html")]
        format: String,
    },
}

const DEFAULT_HTML_OUT: &str = "research/shared/eval-data/vy-v16/metrics.html";
const DEFAULT_MD_OUT: &str = "research/shared/eval-data/vy-v16/metrics.md";

/// Run a dashboard subcommand.
pub fn run(cmd: DashboardCommands) -> Result<(), String> {
    match cmd {
        DashboardCommands::Render {
            results_dir,
            out,
            format,
        } => render_to_file(&results_dir, out.as_deref(), &format),
    }
}

fn render_to_file(
    results_dir: &std::path::Path,
    out_override: Option<&std::path::Path>,
    format: &str,
) -> Result<(), String> {
    let results = load_results_from_dir(results_dir).map_err(|e| e.to_string())?;
    let model = build_model(&results, &results_dir.display().to_string());
    let (rendered, default_out) = match format {
        "html" => (render_html(&model), DEFAULT_HTML_OUT),
        "markdown" | "md" => (render_markdown(&model), DEFAULT_MD_OUT),
        other => return Err(format!("unknown format: {other}")),
    };
    let out_path = out_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(default_out));
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|e| format!("create parent {}: {e}", parent.display()))?;
    }
    fs::write(&out_path, &rendered).map_err(|e| format!("write {}: {e}", out_path.display()))?;
    println!(
        "Wrote {} ({} runs ingested, {} latest rows, {} trends, {} CQs).",
        out_path.display(),
        model.total_runs,
        model.latest_per_being_battery.len(),
        model.trends.len(),
        model.cq_failures.len(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_format_string_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let err = render_to_file(tmp.path(), None, "yaml").expect_err("unknown format must error");
        assert!(err.contains("unknown format"));
    }

    #[test]
    fn missing_results_dir_returns_io_error() {
        let nonexistent = std::path::Path::new("/nonexistent/path/from/ex4336");
        let err = render_to_file(nonexistent, None, "html").expect_err("missing dir must error");
        assert!(err.contains("/nonexistent/path/from/ex4336"));
    }
}
