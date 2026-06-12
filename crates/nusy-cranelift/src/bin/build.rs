//! nusy-graph-build — Graph-native workspace build CLI.
//!
//! EX-3181: Ingests a workspace into a code graph and compiles all function
//! bodies to WASM modules via the cached compiler. Optionally runs tests.
//!
//! # Usage
//!
//! ```bash
//! nusy-graph-build                          # incremental build from graph
//! nusy-graph-build --clean                  # clean build (ignore cache)
//! nusy-graph-build --crate nusy-arrow-core  # build one crate
//! nusy-graph-build --test                   # build + run tests
//! ```

use std::path::PathBuf;

use clap::Parser;
use nusy_cranelift::build_orchestrator::{BuildConfig, BuildOrchestrator};

#[derive(Parser)]
#[command(
    name = "nusy-graph-build",
    about = "Graph-native workspace build — compile all functions to WASM from code graph",
    version
)]
struct Cli {
    /// Workspace root directory (defaults to current directory).
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// Clean build — ignore compilation cache.
    #[arg(long)]
    clean: bool,

    /// Build only a specific crate.
    #[arg(long, name = "crate")]
    crate_filter: Option<String>,

    /// Run tests after building.
    #[arg(long, alias = "test")]
    tests: bool,

    /// Stop on first test failure.
    #[arg(long)]
    fail_fast: bool,
}

fn main() {
    let cli = Cli::parse();

    let config = BuildConfig {
        fail_fast: cli.fail_fast,
        run_tests: cli.tests,
        clean: cli.clean,
        crate_filter: cli.crate_filter,
        graph_path: None,
        function_filter: None,
    };

    eprintln!("Ingesting workspace at {} ...", cli.workspace.display());

    let mut orchestrator = BuildOrchestrator::new().unwrap_or_else(|e| {
        eprintln!("Failed to initialize build orchestrator: {e}");
        std::process::exit(1);
    });

    match orchestrator.build(&cli.workspace, &config) {
        Ok(report) => {
            println!("{}", report.format());

            // Exit with error code if there were compile errors or test failures
            let has_test_failures = report.test_reports.iter().any(|tr| !tr.failed.is_empty());

            if report.compile_errors > 0 || has_test_failures {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Build failed: {e}");
            std::process::exit(1);
        }
    }
}
