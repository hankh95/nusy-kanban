//! nusy-graph-test — Graph-native test discovery and execution CLI.
//!
//! EX-3180 Phase 3: Discovers `#[test]` functions from an ingested code graph
//! and optionally runs them in the WASM sandbox.
//!
//! # Usage
//!
//! ```bash
//! # Discover tests in the graph (no execution)
//! nusy-graph-test discover --graph /path/to/self-graph/
//!
//! # Run tests for one crate
//! nusy-graph-test run --graph /path/to/self-graph/ --crate nusy-arrow-core
//!
//! # Run all tests
//! nusy-graph-test run --graph /path/to/self-graph/ --workspace
//!
//! # Find tests for a specific function
//! nusy-graph-test discover --graph /path/to/self-graph/ --function add_triple
//! ```

use std::path::{Path, PathBuf};

use arrow::array::RecordBatch;
use clap::{Parser, Subcommand};
use nusy_codegraph::test_discovery::{
    discover_tests, discover_tests_for_function, discovery_summary,
};
use nusy_cranelift::CachedWasmCompiler;
use nusy_cranelift::test_runner::{run_all_tests, run_tests_for_crate};
use nusy_cranelift::wasm_compiler::WasmCompiler;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

#[derive(Parser)]
#[command(
    name = "nusy-graph-test",
    about = "Graph-native test discovery and execution",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Discover tests in the code graph without executing them.
    Discover {
        /// Path to the ingested graph directory (contains nodes.parquet).
        #[arg(long)]
        graph: PathBuf,

        /// Show tests for a specific function (incremental test selection).
        #[arg(long)]
        function: Option<String>,
    },

    /// Run tests from the code graph in the WASM sandbox.
    Run {
        /// Path to the ingested graph directory (contains nodes.parquet).
        #[arg(long)]
        graph: PathBuf,

        /// Run tests for a specific crate only.
        #[arg(long, name = "crate")]
        crate_name: Option<String>,

        /// Run tests for all crates in the graph.
        #[arg(long)]
        workspace: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Discover { graph, function } => {
            let batches = load_nodes(&graph);

            if let Some(fn_name) = function {
                let tests = discover_tests_for_function(&batch_refs(&batches), &fn_name);
                if tests.is_empty() {
                    println!("No tests found referencing '{fn_name}'");
                } else {
                    println!("Tests referencing '{fn_name}':");
                    for t in &tests {
                        println!("  {} ({})", t.name, t.id);
                    }
                    println!("\n{} test(s) found", tests.len());
                }
            } else {
                let tests = discover_tests(&batch_refs(&batches));
                let summary = discovery_summary(&tests);
                print!("{summary}");
            }
        }
        Commands::Run {
            graph,
            crate_name,
            workspace,
        } => {
            let batches = load_nodes(&graph);
            let tests = discover_tests(&batch_refs(&batches));
            let summary = discovery_summary(&tests);
            eprintln!("{summary}");

            let compiler = WasmCompiler::new().expect("WasmCompiler init");
            let mut cached = CachedWasmCompiler::new(compiler);

            if let Some(name) = crate_name {
                let nodes = tests.get(name.as_str()).cloned().unwrap_or_default();
                if nodes.is_empty() {
                    eprintln!("No tests found for crate '{name}'");
                    std::process::exit(1);
                }
                let report = run_tests_for_crate(&mut cached, &name, &nodes);
                println!("{}", report.format());
                if !report.failed.is_empty() {
                    std::process::exit(1);
                }
            } else if workspace {
                let reports = run_all_tests(&mut cached, &tests);
                let mut total_passed = 0;
                let mut total_failed = 0;
                let mut total_skipped = 0;
                for report in &reports {
                    println!("{}", report.format());
                    println!();
                    total_passed += report.passed;
                    total_failed += report.failed.len();
                    total_skipped += report.skipped;
                }
                println!(
                    "Overall: {} passed, {} failed, {} skipped",
                    total_passed, total_failed, total_skipped
                );
                if total_failed > 0 {
                    std::process::exit(1);
                }
            } else {
                eprintln!("Specify --crate <name> or --workspace");
                std::process::exit(1);
            }
        }
    }
}

fn load_nodes(graph_dir: &Path) -> Vec<RecordBatch> {
    let nodes_path = graph_dir.join("nodes.parquet");
    if !nodes_path.exists() {
        eprintln!(
            "Graph not found: {}. Run nusy-codegraph-ingest first.",
            nodes_path.display()
        );
        std::process::exit(1);
    }
    let file = std::fs::File::open(&nodes_path)
        .unwrap_or_else(|e| panic!("{}: {e}", nodes_path.display()));
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(file).unwrap_or_else(|e| panic!("parquet: {e}"));
    let reader = builder.build().unwrap_or_else(|e| panic!("reader: {e}"));
    reader.map(|b| b.expect("batch")).collect()
}

fn batch_refs(batches: &[RecordBatch]) -> Vec<RecordBatch> {
    batches.to_vec()
}
