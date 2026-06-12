//! nusy-codegraph-ingest — Self-ingest CLI for the NuSy Rust workspace.
//!
//! # Usage
//!
//! ```bash
//! # Ingest all crates in the NuSy workspace
//! nusy-codegraph-ingest ingest --workspace /path/to/nusy-product-team \
//!                               --output /path/to/self-graph/
//!
//! # Ingest a single crate directory
//! nusy-codegraph-ingest ingest --crate /path/to/nusy-arrow-core \
//!                               --output /path/to/output/
//!
//! # Verify an existing graph for coherence
//! nusy-codegraph-ingest verify --graph /path/to/self-graph/
//! ```

use clap::{Parser, Subcommand};
use nusy_codegraph::ingest_pipeline::{ingest_workspace, verify_graph, write_graph_parquet};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "nusy-codegraph-ingest",
    about = "Ingest NuSy Rust crates into a CodeGraph and verify coherence",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ingest Rust source files into a CodeGraph and write to Parquet.
    ///
    /// Use --workspace to ingest all crates in a Cargo workspace, or
    /// --crate to ingest a single crate directory.
    Ingest {
        /// Path to the Cargo workspace root (contains top-level Cargo.toml with [workspace]).
        #[arg(long, conflicts_with = "crate_dir")]
        workspace: Option<PathBuf>,

        /// Path to a single crate directory to ingest (alternative to --workspace).
        #[arg(long = "crate", conflicts_with = "workspace")]
        crate_dir: Option<PathBuf>,

        /// Output directory for nodes.parquet and edges.parquet.
        #[arg(long)]
        output: PathBuf,

        /// Print verbose per-crate statistics.
        #[arg(long, default_value_t = false)]
        verbose: bool,
    },

    /// Verify graph coherence of an existing CodeGraph Parquet snapshot.
    ///
    /// Checks for duplicate node IDs, dangling edge sources, and dangling
    /// edge targets (excluding intentional ext: external references).
    Verify {
        /// Directory containing nodes.parquet and edges.parquet.
        #[arg(long)]
        graph: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Ingest {
            workspace,
            crate_dir,
            output,
            verbose,
        } => {
            run_ingest(workspace, crate_dir, output, verbose);
        }
        Commands::Verify { graph } => {
            run_verify(graph);
        }
    }
}

fn run_ingest(
    workspace: Option<PathBuf>,
    crate_dir: Option<PathBuf>,
    output: PathBuf,
    verbose: bool,
) {
    let root = match (workspace, crate_dir) {
        (Some(ws), None) => ws,
        (None, Some(cr)) => cr,
        _ => {
            eprintln!("error: must specify --workspace or --crate");
            std::process::exit(1);
        }
    };

    if !root.exists() {
        eprintln!("error: path does not exist: {}", root.display());
        std::process::exit(1);
    }

    eprintln!("nusy-codegraph-ingest: ingesting {}", root.display());

    let result = ingest_workspace(&root);

    eprintln!("{}", result.summary());

    if verbose {
        let mut crate_names: Vec<&str> =
            result.crates.keys().map(|s: &String| s.as_str()).collect();
        crate_names.sort();
        for name in crate_names {
            let cr = &result.crates[name];
            eprintln!(
                "  {name}: {} nodes, {} edges, {} errors",
                cr.nodes.len(),
                cr.edges.len(),
                cr.errors.len()
            );
        }
    }

    // Build merged batches
    let nodes_batch: arrow::array::RecordBatch = match result.merged_nodes_batch() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: failed to build CodeNodes batch: {e}");
            std::process::exit(1);
        }
    };
    let edges_batch: arrow::array::RecordBatch = match result.merged_edges_batch() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: failed to build CodeEdges batch: {e}");
            std::process::exit(1);
        }
    };

    // Quick coherence check
    let violations = verify_graph(&nodes_batch, &edges_batch);
    eprint!("{}", violations.report());

    // Write to Parquet
    if let Err(e) = write_graph_parquet(&nodes_batch, &edges_batch, &output) {
        eprintln!("error: failed to write Parquet: {e}");
        std::process::exit(1);
    }

    eprintln!(
        "nusy-codegraph-ingest: wrote {nodes} nodes + {edges} edges to {out}",
        nodes = nodes_batch.num_rows(),
        edges = edges_batch.num_rows(),
        out = output.display()
    );

    if violations.is_clean() {
        eprintln!("Graph coherence: PASS");
    } else {
        eprintln!("Graph coherence: warnings (see above)");
    }
}

fn run_verify(graph_dir: PathBuf) {
    let nodes_path = graph_dir.join("nodes.parquet");
    let edges_path = graph_dir.join("edges.parquet");

    if !nodes_path.exists() {
        eprintln!("error: nodes.parquet not found in {}", graph_dir.display());
        std::process::exit(1);
    }
    if !edges_path.exists() {
        eprintln!("error: edges.parquet not found in {}", graph_dir.display());
        std::process::exit(1);
    }

    let nodes_batch = read_parquet_first_batch(&nodes_path);
    let edges_batch = read_parquet_first_batch(&edges_path);

    eprintln!(
        "Loaded: {} nodes, {} edges",
        nodes_batch.num_rows(),
        edges_batch.num_rows()
    );

    let violations = verify_graph(&nodes_batch, &edges_batch);
    print!("{}", violations.report());

    if !violations.is_clean() {
        std::process::exit(1);
    }
}

fn read_parquet_first_batch(path: &std::path::Path) -> arrow::array::RecordBatch {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    let file = File::open(path).unwrap_or_else(|e| {
        eprintln!("error: cannot open {}: {e}", path.display());
        std::process::exit(1);
    });
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap_or_else(|e| {
        eprintln!("error: cannot read Parquet {}: {e}", path.display());
        std::process::exit(1);
    });
    let mut reader = builder.build().unwrap_or_else(|e| {
        eprintln!("error: cannot build reader {}: {e}", path.display());
        std::process::exit(1);
    });

    let mut batches: Vec<arrow::array::RecordBatch> = Vec::new();
    for batch in &mut reader {
        match batch {
            Ok(b) => batches.push(b),
            Err(e) => {
                eprintln!("warning: batch read error: {e}");
            }
        }
    }

    if batches.is_empty() {
        eprintln!("error: no data in {}", path.display());
        std::process::exit(1);
    }

    // Concatenate all batches
    arrow::compute::concat_batches(&batches[0].schema(), &batches).unwrap_or_else(|e| {
        eprintln!("error: concat batches: {e}");
        std::process::exit(1);
    })
}
