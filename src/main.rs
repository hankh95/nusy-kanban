//! nusy-kanban CLI — Arrow-native kanban engine.
//!
//! Wraps the nusy-kanban library with a clap-based CLI providing
//! command parity with yurtle-kanban.

use arrow::array::Array;
use clap::{Parser, Subcommand};
use nusy_arrow_git::commit::Commit;
use nusy_arrow_git::save::{persist_commits, restore_commits};
use nusy_kanban::backup::{self, BackupConfig};
use nusy_kanban::config::ConfigFile;
use nusy_kanban::critical_path;
use nusy_kanban::crud::CreateItemInput;
use nusy_kanban::display;
use nusy_kanban::export;
use nusy_kanban::id_alloc;
use nusy_kanban::item_type::ItemType;
use nusy_kanban::persist;
use nusy_kanban::query;
use nusy_kanban::state_machine;
use std::path::PathBuf;
use std::process;

/// Generate a body template for a given item type using SHACL shapes.
fn generate_template(item_type: &str, title: &str, root: &std::path::Path) -> String {
    let loader = nusy_kanban::templates::ShapeLoader::new(root);
    let generator = nusy_kanban::templates::TemplateGenerator::new(loader);

    if let Some(it) = ItemType::from_str_loose(item_type) {
        generator.generate(&it, title)
    } else {
        format!("# {title}\n")
    }
}

#[derive(Parser)]
#[command(name = "nusy-kanban", about = "Arrow-native kanban engine for NuSy")]
struct Cli {
    /// Working directory (defaults to current directory)
    #[arg(long, default_value = ".")]
    root: PathBuf,

    /// NATS server URL for remote mode (e.g., nats://mini:4222)
    #[arg(long)]
    server: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new item
    Create {
        /// Item type (expedition, chore, voyage, etc.)
        item_type: String,
        /// Title for the new item
        title: String,
        /// Priority (low, medium, high, critical)
        #[arg(long)]
        priority: Option<String>,
        /// Assign to an agent
        #[arg(long)]
        assign: Option<String>,
        /// Tags (comma-separated)
        #[arg(long)]
        tags: Option<String>,
        /// Body content (inline text)
        #[arg(long)]
        body: Option<String>,
        /// Read body content from a file
        #[arg(long)]
        body_file: Option<String>,
        /// Read body content from stdin
        #[arg(long)]
        body_stdin: bool,
        /// Use a built-in body template (expedition, chore, voyage)
        #[arg(long)]
        template: Option<String>,
        /// Push after creating (atomic create + commit + push)
        #[arg(long)]
        push: bool,
    },

    /// Move an item to a new status
    Move {
        /// Item ID (e.g., EXP-1257)
        id: String,
        /// Target status
        status: String,
        /// Assign to an agent
        #[arg(long)]
        assign: Option<String>,
        /// Force move (bypass WIP limits)
        #[arg(long)]
        force: bool,
        /// Resolution reason (completed, superseded, wont_do, duplicate, obsolete, merged)
        #[arg(long)]
        resolution: Option<String>,
        /// Provenance URI for closure (e.g., PROP-2025, PR URL)
        #[arg(long)]
        closed_by: Option<String>,
    },

    /// Update fields on an existing item
    Update {
        /// Item ID (e.g., EXP-1274)
        id: String,
        /// New title
        #[arg(long)]
        title: Option<String>,
        /// New priority (low, medium, high, critical)
        #[arg(long)]
        priority: Option<String>,
        /// New assignee
        #[arg(long)]
        assign: Option<String>,
        /// New tags (comma-separated, replaces existing)
        #[arg(long)]
        tags: Option<String>,
        /// New body content (inline)
        #[arg(long)]
        body: Option<String>,
        /// New body content from file
        #[arg(long)]
        body_file: Option<String>,
        /// New related items (comma-separated, replaces existing)
        #[arg(long)]
        related: Option<String>,
        /// New depends-on items (comma-separated, replaces existing)
        #[arg(long)]
        depends_on: Option<String>,
    },

    /// Add a comment to an item
    Comment {
        /// Item ID
        id: String,
        /// Comment text
        text: String,
    },

    /// List items with optional filters
    List {
        /// Filter by status
        #[arg(long)]
        status: Option<String>,
        /// Filter by board (development, research)
        #[arg(long)]
        board: Option<String>,
        /// Filter by item type
        #[arg(long, name = "type")]
        item_type: Option<String>,
        /// Filter by assignee
        #[arg(long)]
        assignee: Option<String>,
        /// Filter by resolution (completed, superseded, wont_do, duplicate, obsolete, merged)
        #[arg(long)]
        resolution: Option<String>,
        /// Filter by priority (critical, high, medium, low)
        #[arg(long)]
        priority: Option<String>,
        /// Filter by tag (exact match, multiple --tag flags = AND)
        #[arg(long)]
        tag: Vec<String>,
        /// Show only items with all dependencies met (unblocked)
        #[arg(long)]
        ready: bool,
    },

    /// Show full board view
    Board {
        /// Board to display (development, research)
        #[arg(long, default_value = "development")]
        board: String,
    },

    /// Show full item details
    Show {
        /// Item ID
        id: String,
        /// Output format: default (metadata + body), md (full markdown), json
        #[arg(long, default_value = "default")]
        format: String,
        /// Show all relationships (depends_on, related, RelationsStore edges)
        #[arg(long)]
        relations: bool,
    },

    /// Search items with natural language or structured queries
    Query {
        /// Query string (NL or structured)
        query: Vec<String>,
        /// Semantic search across item titles and tags
        #[arg(long)]
        search: Option<String>,
        /// SPARQL-like filter (subset: SELECT/WHERE/FILTER/ORDER BY/LIMIT)
        #[arg(long)]
        sparql: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show query decomposition
        #[arg(long)]
        verbose: bool,
        /// Disable semantic search (text-only mode)
        #[arg(long)]
        no_semantic: bool,
        /// Limit results (default 20)
        #[arg(long, default_value = "20")]
        top: usize,
        /// Embedding provider: hash (default), ollama, subprocess
        #[arg(long)]
        embedding_provider: Option<String>,
    },

    /// Show board statistics
    Stats {
        /// Board to show stats for
        #[arg(long, default_value = "development")]
        board: String,
        /// Show weekly velocity (items/week for last N weeks, default 4)
        #[arg(long)]
        velocity: bool,
        /// Show burndown chart (items remaining over time)
        #[arg(long)]
        burndown: bool,
        /// Show per-agent throughput
        #[arg(long)]
        by_agent: bool,
        /// Start date for burndown (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Number of weeks for velocity (default 4)
        #[arg(long, default_value = "4")]
        weeks: u32,
    },

    /// Show recently completed items
    History {
        /// Show items completed this week
        #[arg(long)]
        week: bool,
        /// Show items completed this month (last 30 days)
        #[arg(long)]
        month: bool,
        /// Show items completed since date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Filter by assignee
        #[arg(long)]
        by_assignee: Option<String>,
    },

    /// Show roadmap (voyage-grouped, dependency-ordered view)
    Roadmap {
        /// Legacy flat view (backlog items only)
        #[arg(long)]
        flat: bool,
        /// Show only items with all dependencies met
        #[arg(long)]
        ready: bool,
    },

    /// Show critical path (dependency chain with parallel tracks)
    CriticalPath,

    /// Show agent work assignments based on dependency readiness
    Worklist {
        /// Agent names (comma-separated). Default: DGX,M5,Mini
        #[arg(long, default_value = "DGX,M5,Mini")]
        agents: String,
        /// How many items deep per agent (default: 3)
        #[arg(long, default_value = "3")]
        depth: usize,
    },

    /// Show blocked items
    Blocked,

    /// Validate SHACL conformance of items against their shape rules
    Validate {
        /// Item ID to validate (e.g. EX-3212); omit to validate a whole board
        id: Option<String>,
        /// Also print suggested fix commands for each violation
        #[arg(long)]
        fix: bool,
        /// Validate all items on a specific board (development or research)
        #[arg(long)]
        board: Option<String>,
        /// Validate all items on all boards
        #[arg(long)]
        all: bool,
        /// Filter by status (e.g. backlog, in_progress)
        #[arg(long)]
        status: Option<String>,
    },

    /// Export board or item in various formats
    Export {
        /// Item ID (for single-item export) or omit for board export
        #[arg(long)]
        id: Option<String>,
        /// Export format: expedition-index, markdown, json, html, research-index, item (default: item)
        #[arg(short, long, default_value = "item")]
        format: String,
        /// Board for board-wide exports (development, research)
        #[arg(long, default_value = "development")]
        board: String,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Get the next available ID for a type
    NextId {
        /// Item type
        item_type: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Migrate markdown files to Arrow store
    Migrate {
        /// Dry run — show what would be migrated without saving
        #[arg(long)]
        dry_run: bool,
    },

    /// HDD (research board) commands
    Hdd {
        #[command(subcommand)]
        command: HddCommands,
    },

    /// Training job queue (GPU training coordination)
    Training {
        #[command(subcommand)]
        command: TrainingCommands,
    },

    /// Set priority rank for an item (Captain ordering)
    Rank {
        /// Item ID
        id: String,
        /// Rank number (lower = higher priority)
        rank: u32,
    },

    /// Suggest the next item to work on
    Next {
        /// Filter by assignee
        #[arg(long)]
        assignee: Option<String>,
    },

    /// Initialize nusy-kanban in the current directory
    Init {
        /// Theme preset: nautical (default), software, hdd (research)
        #[arg(long, default_value = "nautical")]
        theme: String,
    },

    /// List configured boards
    Boards,

    /// Show or generate body templates for item types
    Templates {
        /// Item type to generate template for (omit to list all types)
        item_type: Option<String>,
    },

    /// Start MCP (Model Context Protocol) server over stdio
    McpServer {
        /// NATS server URL (default: nats://localhost:4222)
        #[arg(long, default_value = "nats://localhost:4222")]
        nats_url: String,
    },

    /// Graph-native PR workflow (mirrors gh pr)
    Pr {
        #[command(subcommand)]
        command: nusy_kanban::pr_cli::PrCommands,
    },

    /// Graph-native git operations (push/pull/clone/log/blame/rebase)
    Git {
        #[command(subcommand)]
        command: nusy_kanban::git_cli::GitCommands,
    },

    /// Source code transport over NATS (git bundles)
    Source {
        #[command(subcommand)]
        command: nusy_kanban::source_cli::SourceCommands,
    },

    /// Graph-native workspace build — compile functions to WASM from code graph
    #[cfg(feature = "build")]
    Build {
        /// Build only a specific crate (e.g. nusy-arrow-core)
        #[arg(long = "crate")]
        crate_name: Option<String>,
        /// Clean build — ignore compilation cache
        #[arg(long)]
        clean: bool,
        /// Run tests after building
        #[arg(short = 't', long)]
        test: bool,
        /// Stop on first test failure
        #[arg(long)]
        fail_fast: bool,
        /// Machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Workspace root (default: current directory)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Load code graph from a pre-ingested Parquet directory (e.g. research/shared/self-graph)
        /// instead of re-ingesting the workspace. Falls back to live ingest if the path does not exist.
        #[arg(long)]
        graph: Option<PathBuf>,
        /// Watch the workspace for changes and rebuild automatically (not yet implemented;
        /// currently performs a single build and exits)
        #[arg(long)]
        watch: bool,
    },

    /// Graph-native test runner — execute #[test] functions in WASM sandbox
    #[cfg(feature = "build")]
    Test {
        /// Test only a specific crate (e.g. nusy-arrow-core)
        #[arg(long = "crate")]
        crate_name: Option<String>,
        /// Stop on first failure
        #[arg(long)]
        fail_fast: bool,
        /// Machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Workspace root (default: current directory)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Run only tests whose name contains this substring (e.g. --function my_fn)
        #[arg(long)]
        function: Option<String>,
    },

    /// Reconstruct source files from the code graph (emergency materialization)
    ///
    /// Reads CodeNode bodies from .codegraph/nodes.parquet and writes them to
    /// disk as .rs files. This is the safety net for V14-Cutover — if graph-native
    /// compilation fails, this restores file-based development.
    Materialize {
        /// Output directory (default: workspace root, overwriting existing files)
        #[arg(long)]
        output: Option<PathBuf>,
        /// Verify materialized files by running cargo build after writing
        #[arg(long)]
        verify: bool,
        /// Only materialize files for a specific crate
        #[arg(long = "crate")]
        crate_name: Option<String>,
        /// Code graph directory containing nodes.parquet
        #[arg(long, default_value = ".codegraph")]
        graph: PathBuf,
        /// Show what would be written without writing (dry run)
        #[arg(long)]
        dry_run: bool,
    },

    /// Unified config management — view and modify settings from all sources (VY-3510)
    #[command(subcommand)]
    Config(ConfigCommands),

    /// Snapshot the kanban Arrow store to a timestamped backup directory.
    Backup {
        /// List available snapshots and exit (does not create a backup).
        #[arg(long)]
        list: bool,
        /// Show detailed info about a specific snapshot.
        #[arg(long)]
        inspect: Option<String>,
    },

    /// Restore the kanban Arrow store from a backup snapshot.
    Restore {
        /// Snapshot name to restore (e.g. snapshot-2026-04-07_055839).
        snapshot: String,
        /// Confirm restore — required because this overwrites the live store.
        #[arg(long)]
        force: bool,
    },
}

/// Config subcommands (VY-3510 EX-3512)
#[derive(Subcommand)]
enum ConfigCommands {
    /// List all configuration entries
    List {
        /// Filter by tier: auto, being_approved, captain_only, sealed
        #[arg(long)]
        tier: Option<String>,
        /// Show only sealed (genome) entries
        #[arg(long)]
        sealed: bool,
        /// Filter by source
        #[arg(long)]
        source: Option<String>,
    },
    /// Get a single config entry by key
    Get {
        /// Config key (e.g. "being.domain", "covenant.truthfulness")
        key: String,
    },
    /// Set a config value (respects tier enforcement)
    Set {
        /// Config key
        key: String,
        /// New value
        value: String,
        /// Who is making this change
        #[arg(long, default_value = "Captain")]
        requester: String,
    },
    /// Show diff: runtime config vs genome sealed defaults
    Diff,
}

#[derive(Subcommand)]
enum HddCommands {
    /// Create a paper
    Paper {
        /// Paper title
        title: String,
        #[arg(long)]
        tags: Option<String>,
    },
    /// Create a hypothesis linked to a paper
    Hypothesis {
        /// Hypothesis title
        title: String,
        /// Paper number to link to
        #[arg(long)]
        paper: u32,
        #[arg(long)]
        tags: Option<String>,
    },
    /// Create an experiment linked to a hypothesis
    Experiment {
        /// Experiment title
        title: String,
        /// Hypothesis ID to link to (e.g., H130.1)
        #[arg(long)]
        hypothesis: String,
        #[arg(long)]
        tags: Option<String>,
    },
    /// Create a measure
    Measure {
        /// Measure title
        title: String,
        /// Experiment ID to link to (optional)
        #[arg(long)]
        experiment: Option<String>,
        #[arg(long)]
        tags: Option<String>,
    },
    /// Create an idea
    Idea {
        /// Idea title
        title: String,
        #[arg(long)]
        tags: Option<String>,
    },
    /// Create a literature reference
    Literature {
        /// Literature title
        title: String,
        #[arg(long)]
        tags: Option<String>,
    },
    /// Validate HDD research board integrity
    Validate,
    /// Show paper→hypothesis→experiment→measure registry
    Registry,
    /// Start a new run of an experiment
    Run {
        /// Experiment ID (e.g., EXPR-131.1)
        experiment_id: String,
        /// Agent running the experiment
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show all runs for an experiment
    Status {
        /// Experiment ID (e.g., EXPR-131.1)
        experiment_id: String,
    },
    /// Complete an experiment run with results
    Complete {
        /// Experiment ID (e.g., EXPR-131.1)
        experiment_id: String,
        /// Run number to complete
        #[arg(long)]
        run: u32,
        /// Results as JSON string
        #[arg(long)]
        results: Option<String>,
    },
}

// ── Graph-native build / test commands ───────────────────────────────────────

/// JSON-serializable build report for machine-readable output.
#[cfg(feature = "build")]
#[derive(serde::Serialize)]
struct BuildJson {
    crates: usize,
    total_functions: usize,
    compiled: usize,
    cached: usize,
    compile_errors: usize,
    duration_ms: u64,
    success: bool,
}

/// JSON-serializable test summary for machine-readable output.
#[cfg(feature = "build")]
#[derive(serde::Serialize)]
struct TestJson {
    total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
    duration_ms: u64,
    success: bool,
    failures: Vec<TestFailureJson>,
}

/// A single test failure entry in JSON output.
#[cfg(feature = "build")]
#[derive(serde::Serialize)]
struct TestFailureJson {
    crate_name: String,
    test_name: String,
    message: String,
}

/// Run `nk build` — graph-native workspace build.
///
/// Ingests the workspace, compiles all function bodies to WASM via the cached
/// EX-3502: Reconstruct source files from the code graph.
///
/// Reads File-kind CodeNode bodies from nodes.parquet and writes them to disk.
/// This is the V14-Cutover safety net — restores file-based development if
/// graph-native compilation fails.
/// EX-3512: Unified config CLI — list/get/set/diff.
///
/// Config types are defined in nusy-being::config_store (canonical implementation).
/// This CLI reads from NATS KV or local files. Full NATS integration wired in
/// EX-3514 (awakening integration).
fn run_config_command(cmd: &ConfigCommands) {
    match cmd {
        ConfigCommands::List {
            tier,
            sealed,
            source,
        } => {
            println!("nk config list");
            if *sealed {
                println!("  --sealed: showing genome-sealed entries only");
            }
            if let Some(t) = tier {
                println!("  --tier {t}");
            }
            if let Some(s) = source {
                println!("  --source {s}");
            }
            println!();
            println!("Config sources:");
            println!("  being_config.json  — being runtime parameters");
            println!("  cognitive_params   — Arrow-native with autonomy tiers");
            println!("  nats_kv            — NATS KV ship_config bucket");
            println!("  sealed_genome      — immutable genome (Ed25519 signed)");
            println!("  claude_settings    — .claude/settings.json MCP permissions");
            println!();
            println!("Tiers: auto (T1), being_approved (T2), captain_only (T3), sealed");
            println!();
            println!("Integration pending: EX-3514 (awakening) will wire config_store");
            println!("into the being runtime. Until then, use `nk show` for kanban config.");
        }
        ConfigCommands::Get { key } => {
            println!("nk config get {key}");
            println!("Config store not yet connected to NATS KV. Pending EX-3514.");
        }
        ConfigCommands::Set {
            key,
            value,
            requester,
        } => {
            println!("nk config set {key} = {value} (by {requester})");
            println!();
            // Reject sealed keys immediately (no NATS needed for this check)
            let sealed_prefixes = ["covenant.", "safety.", "identity.", "genome."];
            if sealed_prefixes.iter().any(|p| key.starts_with(p)) {
                eprintln!("Error: sealed entry '{key}' — genome immutable, cannot modify");
                eprintln!("Sealed entries can only be changed by re-sealing the genome:");
                eprintln!("  nk genome seal <being>");
                std::process::exit(1);
            }
            println!("Config store not yet connected to NATS KV. Pending EX-3514.");
        }
        ConfigCommands::Diff => {
            println!("nk config diff — runtime config vs genome sealed defaults");
            println!();
            println!("Requires a sealed genome. Generate one with:");
            println!("  nk genome seal <being>");
            println!();
            println!("Config store not yet connected to NATS KV. Pending EX-3514.");
        }
    }
}

/// Run `nk backup` — snapshot or list the kanban Arrow store.
fn run_backup_command(
    root: &std::path::Path,
    list: bool,
    inspect: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = BackupConfig::default();

    if list {
        let snapshots = backup::list_snapshots(&config)?;
        if snapshots.is_empty() {
            println!("No snapshots found in {:?}", config.destination);
        } else {
            println!("Snapshots in {:?}:", config.destination);
            for snap in &snapshots {
                println!(
                    "  {}  version={}  commits={}",
                    snap.name, snap.version, snap.commit_count
                );
            }
        }
        return Ok(());
    }

    if let Some(snapshot_name) = inspect {
        let snapshots = backup::list_snapshots(&config)?;
        let found = snapshots.iter().find(|s| s.name == snapshot_name);
        if let Some(snap) = found {
            println!("Snapshot: {}", snap.name);
            println!("  path: {:?}", snap.path);
            if let Some(created) = &snap.created_at {
                println!("  created: {}", created);
            }
            println!("  version: {}", snap.version);
            println!("  commits: {}", snap.commit_count);
        } else {
            return Err(format!("Snapshot '{}' not found", snapshot_name).into());
        }
        return Ok(());
    }

    // Default: create a new snapshot
    let snapshot_dir = backup::create_snapshot(&config, root)?;
    println!(
        "Backup created: {}",
        snapshot_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    );
    Ok(())
}

/// Run `nk restore` — restore the kanban Arrow store from a snapshot.
fn run_restore_command(
    root: &std::path::Path,
    snapshot: &str,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = BackupConfig::default();
    let restored = backup::restore_snapshot(snapshot, &config, root, force)?;
    println!(
        "Restored from: {}",
        restored.file_name().unwrap_or_default().to_string_lossy()
    );
    Ok(())
}

fn run_materialize_command(
    workspace_root: &std::path::Path,
    graph_dir: &std::path::Path,
    output_dir: Option<&std::path::Path>,
    crate_filter: Option<&str>,
    verify: bool,
    dry_run: bool,
) {
    use arrow::array::{Array, AsArray, RecordBatch, StringArray};
    use arrow::datatypes::DataType;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let parquet_path = if graph_dir.is_absolute() {
        graph_dir.join("nodes.parquet")
    } else {
        workspace_root.join(graph_dir).join("nodes.parquet")
    };

    if !parquet_path.exists() {
        eprintln!("Error: {} not found", parquet_path.display());
        eprintln!("Run `nusy-codegraph-ingest ingest --workspace . --output .codegraph` first.");
        std::process::exit(1);
    }

    let target_dir = output_dir.unwrap_or(workspace_root);
    let t0 = std::time::Instant::now();

    // Read nodes.parquet
    let file = std::fs::File::open(&parquet_path).expect("open parquet");
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).expect("parquet reader");
    let reader = builder.build().expect("build reader");

    let mut files_written = 0u64;
    let mut bytes_written = 0u64;
    let mut errors = 0u64;

    for batch_result in reader {
        let batch: RecordBatch = batch_result.expect("read batch");
        let schema = batch.schema();

        let kind_idx = schema.index_of("kind").expect("kind column");
        let fp_idx = schema.index_of("file_path").expect("file_path column");
        let body_idx = schema.index_of("body").expect("body column");

        // Cast dictionary columns to string
        let kind_col = if matches!(
            batch.column(kind_idx).data_type(),
            DataType::Dictionary(_, _)
        ) {
            arrow::compute::cast(batch.column(kind_idx), &DataType::Utf8).expect("cast kind")
        } else {
            batch.column(kind_idx).clone()
        };
        let fp_col = if matches!(batch.column(fp_idx).data_type(), DataType::Dictionary(_, _)) {
            arrow::compute::cast(batch.column(fp_idx), &DataType::Utf8).expect("cast fp")
        } else {
            batch.column(fp_idx).clone()
        };

        let kinds = kind_col.as_any().downcast_ref::<StringArray>();
        let fps = fp_col.as_any().downcast_ref::<StringArray>();

        // Body can be Utf8 or LargeUtf8
        let body_strings: Vec<Option<String>> = match batch.column(body_idx).data_type() {
            DataType::LargeUtf8 => {
                let arr = batch.column(body_idx).as_string::<i64>();
                (0..arr.len())
                    .map(|i| {
                        if arr.is_null(i) {
                            None
                        } else {
                            Some(arr.value(i).to_string())
                        }
                    })
                    .collect()
            }
            DataType::Utf8 => {
                let arr = batch.column(body_idx).as_string::<i32>();
                (0..arr.len())
                    .map(|i| {
                        if arr.is_null(i) {
                            None
                        } else {
                            Some(arr.value(i).to_string())
                        }
                    })
                    .collect()
            }
            _ => continue,
        };

        let (Some(kinds), Some(fps)) = (kinds, fps) else {
            continue;
        };

        for i in 0..batch.num_rows() {
            if kinds.is_null(i) {
                continue;
            }
            let kind = kinds.value(i);
            if kind != "file" && kind != "File" {
                continue;
            }

            if fps.is_null(i) {
                continue;
            }
            let file_path = fps.value(i);
            if file_path.is_empty() {
                continue;
            }

            // Apply crate filter
            if let Some(filter) = crate_filter {
                if !file_path.contains(&format!("crates/{}/", filter)) {
                    continue;
                }
            }

            let body = match &body_strings[i] {
                Some(b) if !b.is_empty() => b,
                _ => continue,
            };

            let dest = target_dir.join(file_path);

            if dry_run {
                println!("  [dry-run] {} ({} bytes)", file_path, body.len());
                files_written += 1;
                bytes_written += body.len() as u64;
                continue;
            }

            // Create parent directories
            if let Some(parent) = dest.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!("  ERROR creating {}: {e}", parent.display());
                    errors += 1;
                    continue;
                }
            }

            match std::fs::write(&dest, body) {
                Ok(()) => {
                    files_written += 1;
                    bytes_written += body.len() as u64;
                }
                Err(e) => {
                    eprintln!("  ERROR writing {}: {e}", dest.display());
                    errors += 1;
                }
            }
        }
    }

    let elapsed = t0.elapsed();

    println!("╔══════════════════════════════════════════════════════╗");
    println!("║  nk materialize — graph → files                     ║");
    println!("╚══════════════════════════════════════════════════════╝");
    println!();
    println!(
        "  Files: {}   Bytes: {}   Errors: {}   Time: {:?}",
        files_written,
        humanize_bytes(bytes_written),
        errors,
        elapsed
    );
    println!("  Output: {}", target_dir.display());

    if dry_run {
        println!("  (dry run — no files written)");
        return;
    }

    if verify {
        // If materializing to a separate directory, copy Cargo.toml/Cargo.lock
        // (metadata files not stored in the code graph)
        if output_dir.is_some() {
            println!("  Copying Cargo.toml files from workspace...");
            copy_cargo_metadata(workspace_root, target_dir);
        }

        println!("  Verifying with cargo build...");
        let status = std::process::Command::new("cargo")
            .arg("build")
            .arg("--workspace")
            .current_dir(target_dir)
            .status();
        match status {
            Ok(s) if s.success() => {
                println!("  ✓ cargo build --workspace PASSED");
            }
            Ok(s) => {
                eprintln!("  ✗ cargo build failed (exit {})", s.code().unwrap_or(-1));
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("  ✗ cargo build error: {e}");
                std::process::exit(1);
            }
        }
    }

    if errors > 0 {
        std::process::exit(1);
    }
}

/// Copy Cargo.toml, Cargo.lock, and rust-toolchain.toml from workspace to output dir.
/// These are metadata files not stored in the code graph.
fn copy_cargo_metadata(workspace: &std::path::Path, target: &std::path::Path) {
    // Root workspace files
    for name in &["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        let src = workspace.join(name);
        if src.exists() {
            let dest = target.join(name);
            if let Err(e) = std::fs::copy(&src, &dest) {
                eprintln!("    WARN: failed to copy {name}: {e}");
            }
        }
    }
    // Per-crate Cargo.toml files
    let crates_dir = workspace.join("crates");
    if let Ok(entries) = std::fs::read_dir(&crates_dir) {
        for entry in entries.flatten() {
            let cargo_toml = entry.path().join("Cargo.toml");
            if cargo_toml.exists() {
                let rel = cargo_toml.strip_prefix(workspace).unwrap_or(&cargo_toml);
                let dest = target.join(rel);
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::copy(&cargo_toml, &dest) {
                    eprintln!("    WARN: failed to copy {}: {e}", rel.display());
                }
            }
        }
    }
}

fn humanize_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// compiler, and prints a build report. Exits non-zero on compile errors or
/// (when `config.run_tests`) test failures.
#[cfg(feature = "build")]
fn run_build_command(
    workspace: &std::path::Path,
    config: &nusy_cranelift::build_orchestrator::BuildConfig,
    json: bool,
) {
    use nusy_cranelift::build_orchestrator::BuildOrchestrator;

    if !json {
        eprintln!("nk build: ingesting workspace at {} …", workspace.display());
    }

    let mut orchestrator = match BuildOrchestrator::new() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("nk build: init failed: {e}");
            process::exit(1);
        }
    };

    match orchestrator.build(workspace, config) {
        Ok(report) => {
            let has_failures = report.test_reports.iter().any(|tr| !tr.failed.is_empty());
            let success = report.compile_errors == 0 && !has_failures;

            if json {
                let out = BuildJson {
                    crates: report.crate_reports.len(),
                    total_functions: report.total_functions,
                    compiled: report.compiled,
                    cached: report.cached,
                    compile_errors: report.compile_errors,
                    duration_ms: report.total_duration_ms,
                    success,
                };
                println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
            } else {
                println!("{}", report.format());
                if !report.test_reports.is_empty() {
                    for tr in &report.test_reports {
                        println!("{}", tr.format());
                    }
                }
            }

            if !success {
                process::exit(1);
            }
        }
        Err(e) => {
            if json {
                let out = serde_json::json!({ "success": false, "error": e });
                println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
            } else {
                eprintln!("nk build: {e}");
            }
            process::exit(1);
        }
    }
}

/// Run `nk test` — graph-native test runner.
///
/// Builds with `run_tests = true` and emits test results. Exits non-zero on
/// any actual test failures (compile errors count as skipped, not failed).
#[cfg(feature = "build")]
fn run_test_command(
    workspace: &std::path::Path,
    config: &nusy_cranelift::build_orchestrator::BuildConfig,
    json: bool,
) {
    use nusy_cranelift::build_orchestrator::BuildOrchestrator;

    if !json {
        eprintln!("nk test: ingesting workspace at {} …", workspace.display());
    }

    let mut orchestrator = match BuildOrchestrator::new() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("nk test: init failed: {e}");
            process::exit(1);
        }
    };

    match orchestrator.build(workspace, config) {
        Ok(report) => {
            let mut total = 0usize;
            let mut passed = 0usize;
            let mut failed_count = 0usize;
            let mut skipped = 0usize;
            let mut duration_ms = 0u64;
            let mut failures = Vec::new();

            for tr in &report.test_reports {
                total += tr.total;
                passed += tr.passed;
                failed_count += tr.failed.len();
                skipped += tr.skipped;
                duration_ms += tr.duration_ms;

                for (test_name, result) in &tr.failed {
                    let message = match result {
                        nusy_cranelift::test_runner::TestResult::Failed { message } => {
                            message.clone()
                        }
                        nusy_cranelift::test_runner::TestResult::Timeout => "timeout".to_string(),
                        _ => result.status_str().to_string(),
                    };
                    failures.push(TestFailureJson {
                        crate_name: tr.crate_name.clone(),
                        test_name: test_name.clone(),
                        message,
                    });
                }
            }

            let success = failed_count == 0;

            if json {
                let out = TestJson {
                    total,
                    passed,
                    failed: failed_count,
                    skipped,
                    duration_ms,
                    success,
                    failures,
                };
                println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
            } else {
                for tr in &report.test_reports {
                    println!("{}", tr.format());
                }
                println!(
                    "\nnk test summary: {passed} passed, {failed_count} failed, {skipped} skipped in {duration_ms}ms"
                );
            }

            if !success {
                process::exit(1);
            }
        }
        Err(e) => {
            if json {
                let out = serde_json::json!({ "success": false, "error": e });
                println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
            } else {
                eprintln!("nk test: {e}");
            }
            process::exit(1);
        }
    }
}

#[derive(Subcommand)]
enum TrainingCommands {
    /// Queue a training job
    Queue {
        /// Experiment ID
        experiment_id: String,
        #[arg(long)]
        being: String,
        #[arg(long)]
        corpus: String,
        #[arg(long, default_value = "DGX")]
        machine: String,
    },
    /// List training jobs
    List {
        #[arg(long)]
        status: Option<String>,
    },
    /// Claim the next queued job for this machine
    Claim {
        #[arg(long)]
        machine: Option<String>,
    },
    /// Mark a job as complete
    Complete {
        job_id: String,
        #[arg(long)]
        results: String,
    },
    /// Mark a job as failed
    Fail {
        job_id: String,
        #[arg(long)]
        error: String,
    },
}

// ─────────────────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let root = cli.root.canonicalize().unwrap_or(cli.root);

    // MCP server runs locally — intercept before NATS client dispatch
    #[cfg(feature = "client")]
    if let Commands::McpServer { ref nats_url } = cli.command {
        let nats = cli.server.as_deref().unwrap_or(nats_url);
        match nusy_kanban::mcp_server::McpServer::new(nats) {
            Ok(server) => {
                eprintln!("nusy-kanban MCP server started (stdio transport)");
                if let Err(e) = server.run() {
                    eprintln!("MCP server error: {e}");
                    process::exit(1);
                }
                return;
            }
            Err(e) => {
                eprintln!("Failed to start MCP server: {e}");
                process::exit(1);
            }
        }
    }

    // Build and Test commands always run locally — they operate on the local filesystem.
    // Intercept before NATS client dispatch.
    #[cfg(feature = "build")]
    {
        match &cli.command {
            Commands::Build {
                crate_name,
                clean,
                test,
                fail_fast,
                json,
                workspace,
                graph,
                watch,
            } => {
                if *watch {
                    eprintln!(
                        "nk build --watch: filesystem watching is not yet implemented; \
                         running a single build and exiting"
                    );
                }
                let ws = workspace.clone().unwrap_or(root.clone());
                let config = nusy_cranelift::build_orchestrator::BuildConfig {
                    clean: *clean,
                    run_tests: *test,
                    fail_fast: *fail_fast,
                    crate_filter: crate_name.clone(),
                    graph_path: graph.clone(),
                    function_filter: None,
                };
                run_build_command(&ws, &config, *json);
                return;
            }
            Commands::Test {
                crate_name,
                fail_fast,
                json,
                workspace,
                function,
            } => {
                let ws = workspace.clone().unwrap_or(root.clone());
                let config = nusy_cranelift::build_orchestrator::BuildConfig {
                    clean: false,
                    run_tests: true,
                    fail_fast: *fail_fast,
                    crate_filter: crate_name.clone(),
                    graph_path: None,
                    function_filter: function.clone(),
                };
                run_test_command(&ws, &config, *json);
                return;
            }
            _ => {}
        }
    }

    // Config command runs locally — unified config view.
    if let Commands::Config(config_cmd) = &cli.command {
        run_config_command(config_cmd);
        return;
    }

    // Materialize command runs locally — reconstructs files from code graph.
    if let Commands::Materialize {
        output,
        verify,
        crate_name,
        graph,
        dry_run,
    } = &cli.command
    {
        run_materialize_command(
            &root,
            &graph,
            output.as_deref(),
            crate_name.as_deref(),
            *verify,
            *dry_run,
        );
        return;
    }

    // Backup command runs locally — snapshot or list Arrow store.
    if let Commands::Backup { list, inspect } = &cli.command {
        if let Err(e) = run_backup_command(&root, *list, inspect.clone()) {
            eprintln!("Error: {e}");
            process::exit(1);
        }
        return;
    }

    // Restore command runs locally — restore from snapshot.
    if let Commands::Restore { snapshot, force } = &cli.command {
        if let Err(e) = run_restore_command(&root, snapshot, *force) {
            eprintln!("Error: {e}");
            process::exit(1);
        }
        return;
    }

    // Training commands: use NATS KV when --server is provided, file-based otherwise.
    if let Commands::Training {
        command: ref train_cmd,
    } = cli.command
    {
        if let Some(ref server_url) = cli.server {
            // NATS KV mode — distributed training queue (EX-3313)
            if let Err(e) = run_training_nats(server_url, train_cmd) {
                eprintln!("Error: {e}");
                process::exit(1);
            }
        } else {
            // File-based mode — local training queue (backward compat)
            if let Err(e) = run(root, cli.command) {
                eprintln!("Error: {e}");
                process::exit(1);
            }
        }
        return;
    }

    // Client mode: send commands to NATS server
    #[cfg(feature = "client")]
    if let Some(server_url) = &cli.server {
        match run_client(server_url, &cli.command) {
            Ok(()) => return,
            Err(e) => {
                // When --server is specified, NEVER fall back to local mode.
                // The Arrow store on NATS is the single source of truth.
                // Local fallback serves stale data from the retired yurtle-kanban era.
                eprintln!("Error: {e}");
                process::exit(1);
            }
        }
    }

    if let Err(e) = run(root, cli.command) {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

fn run(root: PathBuf, command: Commands) -> Result<(), Box<dyn std::error::Error>> {
    // Handle init before config loading — init creates the config
    if let Commands::Init { ref theme } = command {
        return run_init(&root, theme);
    }

    // Load config
    let config_path = root.join(".yurtle-kanban/config.yaml");
    let config = if config_path.exists() {
        ConfigFile::from_path(&config_path)?
    } else {
        return Err("No .yurtle-kanban/config.yaml found. Run 'nusy-kanban init' first.".into());
    };

    // Load store
    let mut store = persist::load_store(&root)?;

    match command {
        Commands::Create {
            item_type,
            title,
            priority,
            assign,
            tags,
            body,
            body_file,
            body_stdin,
            template,
            push,
        } => {
            let it = ItemType::from_str_loose(&item_type)
                .ok_or_else(|| format!("Unknown item type: {item_type}"))?;

            let tag_list = tags
                .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();

            // Resolve body content from --body, --body-file, --body-stdin, or --template
            let body_content = if let Some(b) = body {
                Some(b)
            } else if let Some(path) = body_file {
                Some(
                    std::fs::read_to_string(&path)
                        .map_err(|e| format!("Failed to read body file '{}': {}", path, e))?,
                )
            } else if body_stdin {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                if buf.is_empty() { None } else { Some(buf) }
            } else if template.is_some() {
                Some(generate_template(&item_type, &title, &root))
            } else {
                None
            };

            let id = store.create_item(&CreateItemInput {
                title: title.clone(),
                item_type: it,
                priority,
                assignee: assign,
                tags: tag_list,
                related: vec![],
                depends_on: vec![],
                body: body_content,
            })?;

            persist::save_store(&root, &store)?;
            println!("Created {id}: {title}");

            if push {
                // Graph-native commit (queryable audit trail via nusy-arrow-git)
                if let Ok(data_dir) = persist::data_dir(&root) {
                    let mut commits_table = restore_commits(&data_dir)
                        .ok()
                        .flatten()
                        .unwrap_or_default();

                    let last_id = commits_table.all().last().map(|c| c.commit_id.clone());
                    let commit = Commit {
                        commit_id: uuid::Uuid::new_v4().to_string(),
                        parent_ids: last_id.into_iter().collect(),
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                        message: format!("Create {id}: {title}"),
                        author: "nusy-kanban".to_string(),
                    };
                    commits_table.append(commit);
                    if let Err(e) = persist_commits(&commits_table, &data_dir) {
                        eprintln!("  Warning: failed to persist graph-native commit: {e}");
                    }
                }

                // Remote sync via shell git (kept for v1 — remote operations
                // are out of scope for nusy-arrow-git)
                let data_dir = root.join(".nusy-kanban");
                let status = std::process::Command::new("git")
                    .current_dir(&root)
                    .args(["add", &data_dir.to_string_lossy()])
                    .status();
                if status.is_ok() {
                    let msg = format!("Create {id}: {title}");
                    let commit_status = std::process::Command::new("git")
                        .current_dir(&root)
                        .args(["commit", "-m", &msg])
                        .status();
                    if commit_status.is_ok() {
                        // Try push with 3 retries (rebase on conflict)
                        for attempt in 1..=3 {
                            let push_result = std::process::Command::new("git")
                                .current_dir(&root)
                                .args(["push", "origin", "HEAD"])
                                .status();
                            match push_result {
                                Ok(s) if s.success() => {
                                    println!("  (committed and pushed to remote)");
                                    break;
                                }
                                _ if attempt < 3 => {
                                    let _ = std::process::Command::new("git")
                                        .current_dir(&root)
                                        .args(["pull", "--rebase", "origin", "main"])
                                        .status();
                                }
                                _ => {
                                    eprintln!("  Warning: push failed after 3 attempts");
                                }
                            }
                        }
                    }
                }
            }
        }

        Commands::Move {
            id,
            status,
            assign,
            force,
            resolution,
            closed_by,
        } => {
            // Get current item to check board
            let item = store.get_item(&id)?;
            let current_status = item
                .column(nusy_kanban::schema::items_col::STATUS)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("status")
                .value(0);
            let item_type_str = item
                .column(nusy_kanban::schema::items_col::ITEM_TYPE)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("type")
                .value(0);
            let board_name = item
                .column(nusy_kanban::schema::items_col::BOARD)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("board")
                .value(0);

            let board = config.board(board_name)?;

            // Validate resolution before the move
            state_machine::validate_resolution(resolution.as_deref(), &status)?;

            // Validate transition (skip if forced)
            if !force {
                state_machine::validate_transition_for_type(
                    board,
                    current_status,
                    &status,
                    Some(item_type_str),
                )?;

                // Check WIP limits
                let exempt: Vec<&str> = board.wip_exempt_types.iter().map(|s| s.as_str()).collect();
                let count = store.count_at_status(&status, &exempt);
                state_machine::check_wip_limit(board, &status, count, item_type_str)?;
            }

            let old = store.update_status(&id, &status, None, force, None)?;

            // Handle assignee update
            if let Some(assignee) = &assign {
                store.update_assignee(&id, Some(assignee))?;
            }

            // Set resolution if provided
            if let Some(ref res) = resolution {
                store.update_resolution(&id, Some(res))?;
            }

            // Set closed_by if provided
            if let Some(ref cb) = closed_by {
                store.update_closed_by(&id, Some(cb))?;
            }

            persist::save_store(&root, &store)?;

            let mut msg = if force {
                format!("Moved {id} from {old} to {status} (forced)")
            } else {
                format!("Moved {id} from {old} to {status}")
            };
            if let Some(ref res) = resolution {
                msg.push_str(&format!(" [resolution: {res}]"));
            }
            println!("{msg}");
        }

        Commands::Update {
            id,
            title,
            priority,
            assign,
            tags,
            body,
            body_file,
            related,
            depends_on,
        } => {
            // Verify item exists
            let _ = store.get_item(&id)?;

            let mut updated = Vec::new();

            if let Some(t) = &title {
                store.update_title(&id, t)?;
                updated.push("title");
            }
            if let Some(p) = &priority {
                store.update_priority(&id, Some(p))?;
                updated.push("priority");
            }
            if let Some(a) = &assign {
                store.update_assignee(&id, Some(a))?;
                updated.push("assignee");
            }
            if let Some(t) = &tags {
                let tag_list: Vec<String> = t.split(',').map(|s| s.trim().to_string()).collect();
                store.update_tags(&id, &tag_list)?;
                updated.push("tags");
            }
            if let Some(b) = &body {
                store.update_body(&id, Some(b))?;
                updated.push("body");
            }
            if let Some(path) = &body_file {
                let content = std::fs::read_to_string(path)?;
                store.update_body(&id, Some(content.trim()))?;
                updated.push("body");
            }
            if let Some(r) = &related {
                let list: Vec<String> = r.split(',').map(|s| s.trim().to_string()).collect();
                store.update_related(&id, &list)?;
                updated.push("related");
            }
            if let Some(d) = &depends_on {
                let list: Vec<String> = d.split(',').map(|s| s.trim().to_string()).collect();
                store.update_depends_on(&id, &list)?;
                updated.push("depends_on");
            }

            if updated.is_empty() {
                println!(
                    "No fields specified to update. Use --title, --priority, --assign, --tags, --body, --body-file, --related, or --depends-on."
                );
            } else {
                persist::save_store(&root, &store)?;
                println!("Updated {id}: {}", updated.join(", "));
            }
        }

        Commands::Comment { id, text } => {
            // Verify item exists
            let _ = store.get_item(&id)?;

            // Record comment as a run entry
            store.add_comment(&id, &text, None)?;
            persist::save_store(&root, &store)?;
            println!("Comment added to {id}");
        }

        Commands::List {
            status,
            board,
            item_type,
            assignee,
            resolution,
            priority,
            tag,
            ready,
        } => {
            let mut results = store.query_items(
                status.as_deref(),
                item_type.as_deref(),
                board.as_deref(),
                assignee.as_deref(),
            );

            // Post-filter by resolution if specified
            if let Some(ref res_filter) = resolution {
                results.retain(|batch| {
                    let res_col = batch
                        .column(nusy_kanban::schema::items_col::RESOLUTION)
                        .as_any()
                        .downcast_ref::<arrow::array::StringArray>()
                        .expect("resolution column");
                    !res_col.is_null(0) && res_col.value(0) == res_filter.as_str()
                });
            }

            // Post-filter by priority if specified
            if let Some(ref pri_filter) = priority {
                results.retain(|batch| {
                    let pri_col = batch
                        .column(nusy_kanban::schema::items_col::PRIORITY)
                        .as_any()
                        .downcast_ref::<arrow::array::StringArray>()
                        .expect("priority column");
                    !pri_col.is_null(0) && pri_col.value(0) == pri_filter.as_str()
                });
            }

            // Post-filter by tags if specified (AND logic: all tags must match)
            if !tag.is_empty() {
                results.retain(|batch| {
                    let tags_col = batch
                        .column(nusy_kanban::schema::items_col::TAGS)
                        .as_any()
                        .downcast_ref::<arrow::array::ListArray>()
                        .expect("tags column");
                    (0..batch.num_rows()).any(|i| {
                        if tags_col.is_null(i) {
                            return false;
                        }
                        let item_tags = tags_col.value(i);
                        let tag_arr = item_tags
                            .as_any()
                            .downcast_ref::<arrow::array::StringArray>()
                            .expect("tag values");
                        let item_tag_set: std::collections::HashSet<&str> =
                            (0..tag_arr.len()).map(|j| tag_arr.value(j)).collect();
                        tag.iter().all(|t| item_tag_set.contains(t.as_str()))
                    })
                });
            }

            // Post-filter: only items with all dependencies met
            if ready {
                let all_batches = store.query_items(None, None, None, None);
                let items = critical_path::extract_items(&all_batches);
                let cp = critical_path::compute_critical_path(&items)
                    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
                let ready_set: std::collections::HashSet<&str> =
                    cp.ready.iter().map(|s| s.as_str()).collect();
                results.retain(|batch| {
                    let ids = batch
                        .column(nusy_kanban::schema::items_col::ID)
                        .as_any()
                        .downcast_ref::<arrow::array::StringArray>()
                        .expect("id");
                    (0..batch.num_rows()).any(|i| ready_set.contains(ids.value(i)))
                });
            }

            print!("{}", display::format_item_table(&results));
        }

        Commands::Board { board } => {
            let board_config = config.board(&board)?;
            let results = store.query_items(None, None, Some(&board), None);
            print!(
                "{}",
                display::format_board_view(&results, &board_config.states)
            );
        }

        Commands::Show {
            id,
            format,
            relations,
        } => {
            let item = store.get_item(&id)?;
            match format.as_str() {
                "md" => {
                    // Full markdown output (frontmatter + body)
                    print!("{}", export::item_to_markdown(&item, 0));
                }
                "json" => {
                    // JSON output including body
                    let batches = vec![item];
                    print!("{}", export::export_json(&batches));
                }
                _ => {
                    // Default: metadata + body
                    print!("{}", display::format_item_detail(&item));
                }
            }

            if relations {
                print!("{}", format_item_relations(&id, &store));
            }
        }

        Commands::Query {
            query: words,
            search,
            sparql,
            json,
            verbose,
            no_semantic,
            top,
            embedding_provider,
        } => {
            // Handle --search flag (semantic search with substring fallback)
            if let Some(ref search_text) = search {
                let all = store.query_items(None, None, None, None);

                if no_semantic {
                    // Substring-only search
                    let search_lower = search_text.to_lowercase();
                    let matched: Vec<arrow::array::RecordBatch> = all
                        .into_iter()
                        .filter(|batch| {
                            let titles = batch
                                .column(nusy_kanban::schema::items_col::TITLE)
                                .as_any()
                                .downcast_ref::<arrow::array::StringArray>()
                                .expect("title");
                            (0..batch.num_rows())
                                .any(|i| titles.value(i).to_lowercase().contains(&search_lower))
                        })
                        .collect();
                    print!("{}", display::format_item_table(&matched));
                } else {
                    // Semantic search with selected embedding provider
                    let provider =
                        nusy_kanban::embeddings::resolve_provider(embedding_provider.as_deref());
                    let embeddings = nusy_kanban::embeddings::embed_items(&all, provider.as_ref())
                        .unwrap_or_default();
                    let sem_results = nusy_kanban::embeddings::semantic_search(
                        &embeddings,
                        search_text,
                        provider.as_ref(),
                        top,
                    )
                    .unwrap_or_default();

                    // Collect matching IDs with scores, then build ranked results
                    let results: Vec<query::RankedResult> = sem_results
                        .iter()
                        .filter_map(|sr| {
                            // Find the item in batches
                            for batch in &all {
                                let ids = batch
                                    .column(nusy_kanban::schema::items_col::ID)
                                    .as_any()
                                    .downcast_ref::<arrow::array::StringArray>()
                                    .expect("id");
                                let titles = batch
                                    .column(nusy_kanban::schema::items_col::TITLE)
                                    .as_any()
                                    .downcast_ref::<arrow::array::StringArray>()
                                    .expect("title");
                                let types = batch
                                    .column(nusy_kanban::schema::items_col::ITEM_TYPE)
                                    .as_any()
                                    .downcast_ref::<arrow::array::StringArray>()
                                    .expect("type");
                                let statuses = batch
                                    .column(nusy_kanban::schema::items_col::STATUS)
                                    .as_any()
                                    .downcast_ref::<arrow::array::StringArray>()
                                    .expect("status");
                                let priorities = batch
                                    .column(nusy_kanban::schema::items_col::PRIORITY)
                                    .as_any()
                                    .downcast_ref::<arrow::array::StringArray>()
                                    .expect("priority");
                                let assignees = batch
                                    .column(nusy_kanban::schema::items_col::ASSIGNEE)
                                    .as_any()
                                    .downcast_ref::<arrow::array::StringArray>()
                                    .expect("assignee");
                                for i in 0..batch.num_rows() {
                                    if ids.value(i) == sr.id {
                                        return Some(query::RankedResult {
                                            id: sr.id.clone(),
                                            title: titles.value(i).to_string(),
                                            item_type: types.value(i).to_string(),
                                            status: statuses.value(i).to_string(),
                                            priority: if priorities.is_null(i) {
                                                String::new()
                                            } else {
                                                priorities.value(i).to_string()
                                            },
                                            assignee: if assignees.is_null(i) {
                                                String::new()
                                            } else {
                                                assignees.value(i).to_string()
                                            },
                                            score: sr.score,
                                        });
                                    }
                                }
                            }
                            None
                        })
                        .collect();

                    if json {
                        print!("{}", query::format_ranked_results_json(&results));
                    } else {
                        print!("{}", query::format_ranked_results(&results));
                    }
                }
                return Ok(());
            }

            // Handle --sparql flag (expanded parser)
            if let Some(ref sparql_query) = sparql {
                let parsed = query::parse_sparql(sparql_query);
                let all = store.query_items(None, None, None, None);
                let rows = query::execute_sparql(&all, &parsed);

                if json {
                    // JSON output for SPARQL results
                    let items: Vec<String> = rows
                        .iter()
                        .map(|row| {
                            let pairs: Vec<String> = row
                                .iter()
                                .map(|(k, v)| {
                                    format!(r#""{}": "{}""#, k.trim_start_matches('?'), v)
                                })
                                .collect();
                            format!("  {{{}}}", pairs.join(", "))
                        })
                        .collect();
                    println!("[\n{}\n]", items.join(",\n"));
                } else {
                    print!(
                        "{}",
                        query::format_sparql_results(&rows, &parsed.select_vars)
                    );
                }
                return Ok(());
            }

            // Default: hybrid query (NL + structured + semantic)
            let query_str = words.join(" ");

            let filters = query::parse_nl_query(&query_str);

            if verbose {
                print!("{}", query::format_query_decomposition(&filters));
            }

            // If we have an ID pattern, just show that item
            if let Some(id) = &filters.id_pattern {
                match store.get_item(id) {
                    Ok(item) => {
                        print!("{}", display::format_item_detail(&item));
                        return Ok(());
                    }
                    Err(_) => {
                        println!("Item not found: {id}");
                        return Ok(());
                    }
                }
            }

            // Handle relation queries (blockers, dependencies)
            if let Some(ref rq) = filters.relation_query {
                let all_items = store.query_items(None, None, None, None);
                let adj = build_dependency_adjacency(&all_items);

                match rq {
                    query::RelationQuery::BlockersOf(target) => {
                        // Find what blocks this item (reverse: who does target depend on?)
                        let chain =
                            nusy_graph_query::traversal::bfs_with_adjacency(target, &adj, 10);
                        if chain.is_empty() {
                            println!("{target} has no blockers.");
                        } else {
                            let status_map = build_status_map(&store);
                            println!("Blockers of {target}:");
                            for node in &chain {
                                let status =
                                    status_map.get(&node.id).map(|s| s.as_str()).unwrap_or("?");
                                let indent = "  ".repeat(node.depth);
                                println!("  {indent}→ {} [{}]", node.id, status);
                            }
                        }
                    }
                    query::RelationQuery::DependenciesOf(target) => {
                        let chain =
                            nusy_graph_query::traversal::bfs_with_adjacency(target, &adj, 10);
                        if chain.is_empty() {
                            println!("{target} has no dependencies.");
                        } else {
                            println!("Dependencies of {target}:");
                            for node in &chain {
                                let indent = "  ".repeat(node.depth);
                                println!("  {indent}→ {} (depth {})", node.id, node.depth);
                            }
                        }
                    }
                }
                return Ok(());
            }

            // Handle ID range filter (e.g., "expeditions above 3100")
            if let Some(above) = filters.id_above {
                let all_items = store.query_items(
                    filters.status.as_deref(),
                    filters.item_type.as_deref(),
                    filters.board.as_deref(),
                    filters.assignee.as_deref(),
                );
                let filtered: Vec<arrow::array::RecordBatch> = all_items
                    .into_iter()
                    .filter(|batch| {
                        let Some(ids) = batch
                            .column(nusy_kanban::schema::items_col::ID)
                            .as_any()
                            .downcast_ref::<arrow::array::StringArray>()
                        else {
                            return false;
                        };
                        let below = filters.id_below;
                        (0..batch.num_rows()).any(|i| {
                            // Extract numeric suffix from ID (e.g., "EX-3100" → 3100)
                            let id = ids.value(i);
                            id.rsplit('-')
                                .next()
                                .and_then(|n| n.parse::<u32>().ok())
                                .is_some_and(|n| n >= above && below.is_none_or(|b| n <= b))
                        })
                    })
                    .collect();
                print!("{}", display::format_item_table(&filtered));
                return Ok(());
            }

            // Build embeddings for semantic ranking (unless --no-semantic)
            let all = store.query_items(None, None, None, None);
            let (embeddings, provider_box): (
                Option<Vec<_>>,
                Option<Box<dyn nusy_kanban::embeddings::EmbeddingProvider>>,
            );
            if !no_semantic && filters.text_query.is_some() {
                let prov = nusy_kanban::embeddings::resolve_provider(embedding_provider.as_deref());
                let embeds =
                    nusy_kanban::embeddings::embed_items(&all, prov.as_ref()).unwrap_or_default();
                provider_box = Some(prov);
                embeddings = Some(embeds);
            } else {
                provider_box = None;
                embeddings = None;
            }

            let results = query::hybrid_query(
                &all,
                &query_str,
                embeddings.as_deref(),
                provider_box
                    .as_ref()
                    .map(|p| p.as_ref() as &dyn nusy_kanban::embeddings::EmbeddingProvider),
                top,
            );

            if json {
                print!("{}", query::format_ranked_results_json(&results));
            } else if filters.text_query.is_some() {
                // Show ranked output when a text query drove semantic scoring
                print!("{}", query::format_ranked_results(&results));
            } else {
                // Fall back to standard table for pure structural queries
                let filtered = store.query_items(
                    filters.status.as_deref(),
                    filters.item_type.as_deref(),
                    filters.board.as_deref(),
                    filters.assignee.as_deref(),
                );
                let text_filtered: Vec<arrow::array::RecordBatch> =
                    if let Some(text) = &filters.text_query {
                        filtered
                            .into_iter()
                            .filter(|batch| {
                                let titles = batch
                                    .column(nusy_kanban::schema::items_col::TITLE)
                                    .as_any()
                                    .downcast_ref::<arrow::array::StringArray>()
                                    .expect("title");
                                (0..batch.num_rows())
                                    .any(|i| query::text_matches(titles.value(i), text))
                            })
                            .collect()
                    } else {
                        filtered
                    };
                print!("{}", display::format_item_table(&text_filtered));
            }
        }

        Commands::Stats {
            board,
            velocity,
            burndown,
            by_agent,
            since,
            weeks,
        } => {
            if velocity {
                let vel = nusy_kanban::stats::compute_velocity(store.runs_batches(), weeks);
                print!("{}", nusy_kanban::stats::format_velocity(&vel));
            } else if burndown {
                let since_ms = if let Some(ref date) = since {
                    nusy_kanban::stats::parse_date_to_ms(date)
                        .ok_or_else(|| format!("Invalid date: {date} (use YYYY-MM-DD)"))?
                } else {
                    // Default: last 4 weeks
                    chrono::Utc::now().timestamp_millis() - (4 * 7 * 24 * 60 * 60 * 1000)
                };
                let points = nusy_kanban::stats::compute_burndown(
                    store.items_batches(),
                    store.runs_batches(),
                    since_ms,
                );
                print!("{}", nusy_kanban::stats::format_burndown(&points));
            } else if by_agent {
                let stats = nusy_kanban::stats::compute_agent_stats(store.runs_batches());
                print!("{}", nusy_kanban::stats::format_agent_stats(&stats));
            } else {
                // Default: existing status/type summary
                let board_config = config.board(&board)?;
                let results = store.query_items(None, None, Some(&board), None);
                print!("{}", display::format_stats(&results, &board_config.states));
            }
        }

        Commands::History {
            week,
            month,
            since,
            by_assignee,
        } => {
            let since_ms = if let Some(ref date) = since {
                nusy_kanban::stats::parse_date_to_ms(date)
                    .ok_or_else(|| format!("Invalid date: {date} (use YYYY-MM-DD)"))?
            } else if month {
                chrono::Utc::now().timestamp_millis() - (30 * 24 * 60 * 60 * 1000)
            } else if week {
                chrono::Utc::now().timestamp_millis() - (7 * 24 * 60 * 60 * 1000)
            } else {
                // Default: show all done items (use old behavior if no flags)
                0
            };

            if week || month || since.is_some() || by_assignee.is_some() {
                let entries = nusy_kanban::stats::filter_history(
                    store.items_batches(),
                    store.runs_batches(),
                    since_ms,
                    by_assignee.as_deref(),
                );
                print!("{}", nusy_kanban::stats::format_history_entries(&entries));
            } else {
                // Original behavior: show all done items
                let default_board = config.default_board()?;
                let done_status = default_board
                    .states
                    .last()
                    .map(|s| s.as_str())
                    .unwrap_or("done");
                print!(
                    "{}",
                    display::format_history(store.items_batches(), done_status)
                );
            }
        }

        Commands::Roadmap { flat, ready } => {
            let all_batches = store.query_items(None, None, None, None);
            if all_batches.is_empty() {
                println!("No items found.");
            } else if flat {
                // Legacy flat view — backlog only, sorted by priority
                let results = store.query_items(Some("backlog"), None, None, None);
                if results.is_empty() {
                    println!("No backlog items.");
                } else {
                    let mut sorted = results;
                    sorted.sort_by(|a, b| {
                        let get_priority = |batch: &arrow::array::RecordBatch| -> i32 {
                            let prios = batch
                                .column(nusy_kanban::schema::items_col::PRIORITY)
                                .as_any()
                                .downcast_ref::<arrow::array::StringArray>()
                                .expect("priority");
                            if prios.is_null(0) {
                                return 99;
                            }
                            match prios.value(0) {
                                "critical" => 0,
                                "high" => 1,
                                "medium" => 2,
                                "low" => 3,
                                _ => 99,
                            }
                        };
                        get_priority(a).cmp(&get_priority(b))
                    });
                    println!("Roadmap (flat, ranked by priority):");
                    print!("{}", display::format_item_table(&sorted));
                }
            } else {
                // Voyage-grouped, dependency-ordered view
                let items = critical_path::extract_items(&all_batches);
                let cp = critical_path::compute_critical_path(&items)
                    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
                let (groups, orphans) = critical_path::group_by_voyage(&items);

                if ready {
                    // Show only ready items
                    println!("Ready Items ({}):", cp.ready.len());
                    let ready_batches: Vec<_> = cp
                        .ready
                        .iter()
                        .filter_map(|id| store.get_item(id).ok())
                        .collect();
                    if ready_batches.is_empty() {
                        println!("  (none)");
                    } else {
                        print!("{}", display::format_item_table(&ready_batches));
                    }
                } else {
                    print!(
                        "{}",
                        critical_path::format_roadmap(&items, &groups, &orphans, &cp)
                    );
                }
            }
        }

        Commands::CriticalPath => {
            let all_batches = store.query_items(None, None, None, None);
            if all_batches.is_empty() {
                println!("No items found.");
            } else {
                let items = critical_path::extract_items(&all_batches);
                let cp = critical_path::compute_critical_path(&items)
                    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
                print!("{}", critical_path::format_critical_path(&items, &cp));
            }
        }

        Commands::Worklist { agents, depth } => {
            let all_batches = store.query_items(None, None, None, None);
            if all_batches.is_empty() {
                println!("No items found.");
            } else {
                let items = critical_path::extract_items(&all_batches);
                let cp = critical_path::compute_critical_path(&items)
                    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
                let agent_list: Vec<String> =
                    agents.split(',').map(|s| s.trim().to_string()).collect();
                let worklist = critical_path::generate_worklist(&items, &cp, &agent_list, depth);
                print!("{}", critical_path::format_worklist(&worklist));
            }
        }

        Commands::Blocked => {
            let blocked = find_blocked_items(&store);
            if blocked.is_empty() {
                println!("No blocked items.");
            } else {
                println!("Blocked Items ({}):", blocked.len());
                print!("{}", display::format_item_table(&blocked));

                // Show blocker chains for each blocked item
                println!("\nBlocker Chains:");
                println!("{}", "─".repeat(60));
                let all_items = store.query_items(None, None, None, None);
                let adj = build_dependency_adjacency(&all_items);
                let status_map = build_status_map(&store);

                for batch in &blocked {
                    if let Some(ids) = batch
                        .column(nusy_kanban::schema::items_col::ID)
                        .as_any()
                        .downcast_ref::<arrow::array::StringArray>()
                    {
                        for i in 0..batch.num_rows() {
                            let id = ids.value(i);
                            let chain =
                                nusy_graph_query::traversal::bfs_with_adjacency(id, &adj, 10);
                            if !chain.is_empty() {
                                print!("  {id}");
                                for node in &chain {
                                    let status =
                                        status_map.get(&node.id).map(|s| s.as_str()).unwrap_or("?");
                                    print!(" → {} [{}]", node.id, status);
                                }
                                println!();
                            }
                        }
                    }
                }
            }
        }

        Commands::Validate {
            id,
            fix,
            board,
            all,
            status,
        } => {
            use nusy_kanban::validate;

            if let Some(item_id) = id {
                // Single-item validation
                let batch = store.get_item(&item_id)?;
                let report = validate::validate_item(&batch);
                println!("{}", validate::format_report(&report, fix));
                if !report.is_conformant() {
                    process::exit(1);
                }
            } else {
                // Board or all-boards validation
                let board_filter = if all {
                    None
                } else {
                    Some(board.as_deref().unwrap_or("development"))
                };

                let batches = store.query_items(status.as_deref(), None, board_filter, None);
                if batches.is_empty() {
                    println!("No items found.");
                } else {
                    let reports = validate::validate_all(&batches);

                    // Print per-item reports for items with violations
                    let mut any_violations = false;
                    for report in &reports {
                        if !report.violations.is_empty() {
                            println!("{}", validate::format_report(report, fix));
                            println!();
                            any_violations = true;
                        }
                    }

                    println!("{}", validate::format_board_summary(&reports));

                    if any_violations {
                        process::exit(1);
                    }
                }
            }
        }

        Commands::Export {
            id,
            format,
            board,
            output,
        } => {
            let content = match format.as_str() {
                "expedition-index" => {
                    let batches = store.query_items(None, None, Some(&board), None);
                    export::export_board_index(&batches, &board, None)
                }
                "json" => {
                    let batches = store.query_items(None, None, Some(&board), None);
                    export::export_json(&batches)
                }
                "markdown" => {
                    let batches = store.query_items(None, None, Some(&board), None);
                    export::export_markdown_table(&batches)
                }
                "html" => {
                    let batches = store.query_items(None, None, Some(&board), None);
                    // Compute burndown for the last 8 weeks
                    let since_ms =
                        chrono::Utc::now().timestamp_millis() - (8 * 7 * 24 * 60 * 60 * 1000i64);
                    let burndown = nusy_kanban::stats::compute_burndown(
                        store.items_batches(),
                        store.runs_batches(),
                        since_ms,
                    );
                    let points = if burndown.is_empty() {
                        None
                    } else {
                        Some(burndown.as_slice())
                    };
                    export::export_board_html(&batches, &board, None, points)
                }
                "research-index" => {
                    let rel_store = persist::load_relations(&root)?;
                    let chains = nusy_kanban::build_registry(&store, &rel_store);
                    export::export_research_index_html(&chains)
                }
                _ => {
                    if let Some(item_id) = &id {
                        let item = store.get_item(item_id)?;
                        export::item_to_markdown(&item, 0)
                    } else {
                        return Err("--id required for item export format".into());
                    }
                }
            };

            if let Some(out_path) = &output {
                std::fs::write(out_path, &content)?;
                println!("Exported to {out_path}");
            } else {
                print!("{content}");
            }
        }

        Commands::Migrate { dry_run } => {
            let result = nusy_kanban::migrate::migrate_boards(&root, &config)?;
            println!("{}", result.summary());

            if dry_run {
                println!("Dry run — no changes saved.");
            } else {
                let (migrated_store, rel_store) = result.into_stores()?;
                persist::save_store(&root, &migrated_store)?;
                persist::save_relations(&root, &rel_store)?;
                println!("Migration saved to .nusy-kanban/");
            }
        }

        Commands::Hdd { command: hdd_cmd } => {
            let mut rel_store = persist::load_relations(&root)?;
            let parse_tags = |t: Option<String>| -> Vec<String> {
                t.map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                    .unwrap_or_default()
            };

            match hdd_cmd {
                HddCommands::Paper { title, tags } => {
                    let result = nusy_kanban::create_paper(&mut store, &title, parse_tags(tags))?;
                    persist::save_store(&root, &store)?;
                    println!("Created {}: {}", result.id, title);
                }
                HddCommands::Hypothesis { title, paper, tags } => {
                    let result = nusy_kanban::create_hypothesis(
                        &mut store,
                        &mut rel_store,
                        &title,
                        paper,
                        parse_tags(tags),
                    )?;
                    persist::save_store(&root, &store)?;
                    persist::save_relations(&root, &rel_store)?;
                    println!(
                        "Created {}: {} (linked to PAPER-{})",
                        result.id, title, paper
                    );
                }
                HddCommands::Experiment {
                    title,
                    hypothesis,
                    tags,
                } => {
                    let result = nusy_kanban::create_experiment(
                        &mut store,
                        &mut rel_store,
                        &title,
                        &hypothesis,
                        parse_tags(tags),
                    )?;
                    persist::save_store(&root, &store)?;
                    persist::save_relations(&root, &rel_store)?;
                    println!(
                        "Created {}: {} (linked to {})",
                        result.id, title, hypothesis
                    );
                }
                HddCommands::Measure {
                    title,
                    experiment,
                    tags,
                } => {
                    let result = nusy_kanban::create_measure(
                        &mut store,
                        &mut rel_store,
                        &title,
                        experiment.as_deref(),
                        parse_tags(tags),
                    )?;
                    persist::save_store(&root, &store)?;
                    persist::save_relations(&root, &rel_store)?;
                    println!("Created {}: {}", result.id, title);
                }
                HddCommands::Idea { title, tags } => {
                    let result = nusy_kanban::create_idea(&mut store, &title, parse_tags(tags))?;
                    persist::save_store(&root, &store)?;
                    println!("Created {}: {}", result.id, title);
                }
                HddCommands::Literature { title, tags } => {
                    let result =
                        nusy_kanban::create_literature(&mut store, &title, parse_tags(tags))?;
                    persist::save_store(&root, &store)?;
                    println!("Created {}: {}", result.id, title);
                }
                HddCommands::Validate => {
                    let errors = nusy_kanban::validate_hdd(&store, &rel_store);
                    if errors.is_empty() {
                        println!("HDD validation passed — no issues found.");
                    } else {
                        println!("HDD validation found {} issues:", errors.len());
                        for err in &errors {
                            println!("  - {err}");
                        }
                    }
                }
                HddCommands::Registry => {
                    let chains = nusy_kanban::build_registry(&store, &rel_store);
                    if chains.is_empty() {
                        println!("No papers found.");
                    } else {
                        for chain in &chains {
                            println!("📄 {} — {}", chain.paper_id, chain.paper_title);
                            for hyp in &chain.hypotheses {
                                println!("  🔬 {} — {}", hyp.id, hyp.title);
                                for expr in &hyp.experiments {
                                    println!("    🧪 {} — {}", expr.id, expr.title);
                                    for m in &expr.measures {
                                        println!("      📏 {} — {}", m.id, m.title);
                                    }
                                }
                            }
                        }
                    }
                }
                HddCommands::Run {
                    experiment_id,
                    agent,
                } => {
                    let mut run_store = persist::load_experiment_runs(&root);
                    let run_id = run_store.start_run(&experiment_id, agent.as_deref())?;
                    persist::save_experiment_runs(&root, &run_store)?;
                    println!("Started {run_id}");
                }
                HddCommands::Status { experiment_id } => {
                    let run_store = persist::load_experiment_runs(&root);
                    let runs = run_store.list_runs(&experiment_id);
                    print!("{}", nusy_kanban::experiment_runs::format_runs(&runs));
                }
                HddCommands::Complete {
                    experiment_id,
                    run,
                    results,
                } => {
                    let mut run_store = persist::load_experiment_runs(&root);
                    run_store.complete_run(&experiment_id, run, results.as_deref())?;
                    persist::save_experiment_runs(&root, &run_store)?;
                    println!("Completed {experiment_id} run #{run}");
                }
            }
        }

        Commands::Training { command: train_cmd } => {
            let queue_path = root.join(".nusy-kanban/training_queue.json");
            let mut queue = if queue_path.exists() {
                let data = std::fs::read_to_string(&queue_path).unwrap_or_default();
                serde_json::from_str::<nusy_kanban::training_queue::TrainingQueue>(&data)
                    .unwrap_or_default()
            } else {
                nusy_kanban::training_queue::TrainingQueue::new()
            };

            let agent_name = std::env::var("HOSTNAME")
                .or_else(|_| std::env::var("HOST"))
                .unwrap_or_else(|_| "agent".to_string());

            match train_cmd {
                TrainingCommands::Queue {
                    experiment_id,
                    being,
                    corpus,
                    machine,
                } => {
                    let job_id =
                        queue.queue_job(&experiment_id, &being, &corpus, &machine, &agent_name);
                    println!("Queued {job_id}: {experiment_id} on {machine}");
                }
                TrainingCommands::List { status } => {
                    let filter = status
                        .as_deref()
                        .and_then(nusy_kanban::training_queue::JobStatus::parse);
                    print!("{}", queue.format_table_filtered(filter.as_ref()));
                }
                TrainingCommands::Claim { machine } => {
                    let m = machine.as_deref().unwrap_or(&agent_name);
                    if let Some(job) = queue.claim_job(m) {
                        println!(
                            "Claimed {}: {} ({})",
                            job.id, job.payload.experiment_id, job.payload.being
                        );
                    } else {
                        println!("No queued jobs for {m}");
                    }
                }
                TrainingCommands::Complete { job_id, results } => {
                    if queue.complete_job(&job_id, &results) {
                        println!("Completed {job_id}");
                    } else {
                        eprintln!("Failed to complete {job_id} (not running?)");
                    }
                }
                TrainingCommands::Fail { job_id, error } => {
                    if queue.fail_job(&job_id, &error) {
                        println!("Failed {job_id}");
                    } else {
                        eprintln!("Failed to mark {job_id} as failed (not running?)");
                    }
                }
            }

            if let Ok(json) = serde_json::to_string_pretty(&queue) {
                let _ = std::fs::create_dir_all(
                    queue_path.parent().unwrap_or(std::path::Path::new(".")),
                );
                let _ = std::fs::write(&queue_path, json);
            }
        }

        Commands::Rank { id, rank: _rank } => {
            // Verify item exists
            let _item = store.get_item(&id)?;
            // TODO: Add rank column to items schema (schema migration needed).
            // For now, rank is not persisted. Use `move` with --assign to prioritize.
            return Err(format!(
                "rank command not yet implemented — rank column requires schema migration. Item {id} exists but rank was not saved."
            ).into());
        }

        Commands::Next { assignee } => {
            // Find highest-priority unblocked backlog item
            let results = store.query_items(Some("backlog"), None, None, assignee.as_deref());
            if results.is_empty() {
                println!("No backlog items found.");
            } else {
                // Return the first item (already sorted by query)
                let item = &results[0];
                let id = item
                    .column(nusy_kanban::schema::items_col::ID)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .expect("id")
                    .value(0);
                let title = item
                    .column(nusy_kanban::schema::items_col::TITLE)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .expect("title")
                    .value(0);
                println!("Next: {id} — {title}");
            }
        }

        Commands::Init { .. } => {
            // Handled before config loading — should not reach here
            unreachable!("Init is handled before config loading");
        }

        Commands::Boards => {
            for board in &config.boards {
                let items = store.query_items(None, None, Some(&board.name), None);
                println!("{}: {} items", board.name, items.len());
            }
        }

        Commands::Templates { item_type } => {
            let loader = nusy_kanban::templates::ShapeLoader::new(&root);
            let generator = nusy_kanban::templates::TemplateGenerator::new(loader);

            if let Some(type_str) = item_type {
                if let Some(it) = ItemType::from_str_loose(&type_str) {
                    print!("{}", generator.generate(&it, "<Title>"));
                } else {
                    eprintln!("Unknown item type: {type_str}");
                    process::exit(1);
                }
            } else {
                let summaries = generator.list_all();
                print!(
                    "{}",
                    nusy_kanban::templates::format_type_listing(&summaries)
                );
            }
        }

        Commands::McpServer { nats_url } => {
            let server = nusy_kanban::mcp_server::McpServer::new(&nats_url)?;
            eprintln!("nusy-kanban MCP server started (stdio transport)");
            server.run()?;
        }

        Commands::Pr { command: pr_cmd } => {
            let mut proposals = nusy_graph_review::ProposalStore::new();
            let mut comments = nusy_graph_review::CommentStore::new();
            let mut ci_results = nusy_graph_review::CiResultStore::new();
            // TODO: Load proposals from persistence when available
            let agent = detect_agent_name();
            nusy_kanban::pr_cli::run_pr_command(
                &pr_cmd,
                &mut proposals,
                &mut comments,
                &mut ci_results,
                &agent,
            )?;
        }

        Commands::Git { command: git_cmd } => {
            use nusy_kanban::git_cli::GitCommands;
            match git_cmd {
                GitCommands::Log { limit, store: _ } => {
                    println!("nk git log: local mode not yet implemented — use --server");
                    println!("  (hint: nk git log --limit {limit})");
                }
                _ => {
                    println!(
                        "nk git: local mode not yet implemented — use --server for push/pull/clone"
                    );
                }
            }
        }

        Commands::Source { .. } => {
            println!("nk source: requires --server for NATS transport");
        }

        Commands::NextId { item_type, json } => {
            let it = ItemType::from_str_loose(&item_type)
                .ok_or_else(|| format!("Unknown item type: {item_type}"))?;
            let prefix = it.prefix();
            let next = id_alloc::max_id_for_type(store.items_batches(), prefix) + 1;

            if json {
                println!("{}", export::next_id_json(prefix, next));
            } else {
                println!("{}-{}", prefix, next);
            }
        }

        // Build and Test are intercepted before run() is called — they run
        // locally and never reach this match. These arms are unreachable but
        // required for exhaustive pattern matching.
        #[cfg(feature = "build")]
        Commands::Build { .. }
        | Commands::Test { .. }
        | Commands::Materialize { .. }
        | Commands::Config(_)
        | Commands::Backup { .. }
        | Commands::Restore { .. } => {
            unreachable!("Build/Test/Materialize/Config/Backup/Restore intercepted before run()")
        }

        #[cfg(not(feature = "build"))]
        Commands::Materialize { .. }
        | Commands::Config(_)
        | Commands::Backup { .. }
        | Commands::Restore { .. } => {
            return Err("Materialize, Config, Backup, and Restore commands require the 'build' feature. Rebuild with --features build.".into());
        }
    }

    Ok(())
}

/// Handle `init` command — runs before config loading since init creates the config.
/// Uses std::fs for infrastructure bootstrapping (not data persistence).
///
/// Themes:
/// - `nautical` — NuSy default (dev + research boards)
/// - `software` — software dev only (dev board, no research)
/// - `hdd` — research board preset (research board primary)
fn run_init(root: &std::path::Path, theme: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = root.join(".yurtle-kanban");
    if config_dir.exists() {
        println!("Already initialized — .yurtle-kanban/ exists");
        return Ok(());
    }

    std::fs::create_dir_all(&config_dir)?;
    std::fs::write(
        config_dir.join("config.yaml"),
        nusy_kanban::config::default_config_yaml(),
    )?;
    std::fs::create_dir_all(config_dir.join("data"))?;

    // Copy ontology files (kanban.ttl + SHACL shapes)
    let ontology_src = root.join("crates/nusy-kanban/ontology");
    let ontology_dest = config_dir.join("ontology");
    if ontology_src.exists() {
        copy_dir_recursive(&ontology_src, &ontology_dest)?;
    }

    // Generate theme-specific template files
    let loader = nusy_kanban::templates::ShapeLoader::new(root);
    let generator = nusy_kanban::templates::TemplateGenerator::new(loader);
    let templates_dir = config_dir.join("templates");
    std::fs::create_dir_all(&templates_dir)?;

    let types: Vec<ItemType> = match theme {
        "software" => vec![
            ItemType::Expedition,
            ItemType::Voyage,
            ItemType::Chore,
            ItemType::Hazard,
            ItemType::Signal,
            ItemType::Feature,
        ],
        "hdd" => vec![
            ItemType::Paper,
            ItemType::Hypothesis,
            ItemType::Experiment,
            ItemType::Measure,
            ItemType::Idea,
            ItemType::Literature,
        ],
        _ => {
            // nautical = all 12 types
            vec![
                ItemType::Expedition,
                ItemType::Voyage,
                ItemType::Chore,
                ItemType::Hazard,
                ItemType::Signal,
                ItemType::Feature,
                ItemType::Paper,
                ItemType::Hypothesis,
                ItemType::Experiment,
                ItemType::Measure,
                ItemType::Idea,
                ItemType::Literature,
            ]
        }
    };

    for it in &types {
        let template = generator.generate(it, "<Title>");
        let filename = format!("_TEMPLATE_{}.md", it.as_str().to_uppercase());
        std::fs::write(templates_dir.join(&filename), &template)?;
    }

    // Install Claude Code skills (idempotent — updates existing)
    install_claude_skills(root)?;

    println!(
        "Initialized nusy-kanban in .yurtle-kanban/ (theme: {theme}, {} templates)",
        types.len()
    );
    Ok(())
}

/// Recursively copy a directory.
fn copy_dir_recursive(
    src: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            std::fs::copy(&src_path, &dest_path)?;
        }
    }

    Ok(())
}

/// Install Claude Code skills to .claude/skills/ for kanban workflow integration.
/// Idempotent — safe to run multiple times.
fn install_claude_skills(root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let skills_src = root.join("crates/nusy-kanban/skills");
    let skills_dest = root.join(".claude/skills");

    if !skills_src.exists() {
        // Skills not bundled (standalone install) — skip silently
        return Ok(());
    }

    let mut installed = 0;
    for entry in std::fs::read_dir(&skills_src)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }

        let skill_name = entry.file_name();
        let dest_dir = skills_dest.join(&skill_name);
        let src_skill = entry.path().join("SKILL.md");

        if !src_skill.exists() {
            continue;
        }

        // Skip if destination already has a SKILL.md (don't overwrite project-specific skills)
        let dest_skill = dest_dir.join("SKILL.md");
        if dest_skill.exists() {
            continue;
        }

        std::fs::create_dir_all(&dest_dir)?;
        std::fs::copy(&src_skill, &dest_skill)?;
        installed += 1;
    }

    if installed > 0 {
        println!("Installed {installed} Claude Code skills to .claude/skills/");
    }

    Ok(())
}

/// Run training commands via NATS KV (distributed queue).
///
/// EX-3313: When `--server` is provided, training commands use NATS KV instead
/// of the local file-based queue. This enables distributed GPU job coordination
/// without git push/pull for queue state.
fn run_training_nats(
    server_url: &str,
    train_cmd: &TrainingCommands,
) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut nats_queue = nusy_kanban::nats_training_queue::NatsTrainingQueue::new(server_url);
        nats_queue.connect().await?;

        let agent_name = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("HOST"))
            .unwrap_or_else(|_| "agent".to_string());

        match train_cmd {
            TrainingCommands::Queue {
                experiment_id,
                being,
                corpus,
                machine,
            } => {
                let job_id = nats_queue
                    .queue_job(experiment_id, being, corpus, machine, &agent_name)
                    .await?;
                println!("Queued {job_id}: {experiment_id} on {machine}");
            }
            TrainingCommands::List { status } => {
                let jobs = nats_queue.list_jobs(status.as_deref()).await?;
                print!(
                    "{}",
                    nusy_kanban::nats_training_queue::NatsTrainingQueue::format_jobs(&jobs)
                );
            }
            TrainingCommands::Claim { machine } => {
                let m = machine.as_deref().unwrap_or(&agent_name);
                if let Some(job) = nats_queue.claim_job(m).await? {
                    let id = job.get("id").and_then(|v| v.as_str()).unwrap_or("-");
                    let exp = job
                        .pointer("/payload/experiment_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-");
                    let being = job
                        .pointer("/payload/being")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-");
                    println!("Claimed {id}: {exp} ({being})");
                } else {
                    println!("No queued jobs for {m}");
                }
            }
            TrainingCommands::Complete { job_id, results } => {
                if nats_queue.complete_job(job_id, results).await? {
                    println!("Completed {job_id}");
                } else {
                    eprintln!("Failed to complete {job_id} (not running?)");
                }
            }
            TrainingCommands::Fail { job_id, error } => {
                if nats_queue.fail_job(job_id, error).await? {
                    println!("Failed {job_id}");
                } else {
                    eprintln!("Failed to mark {job_id} as failed (not running?)");
                }
            }
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

/// Run a command in client mode via NATS request-reply.
#[cfg(feature = "client")]
fn run_client(server_url: &str, command: &Commands) -> Result<(), Box<dyn std::error::Error>> {
    let client = nusy_kanban::client::NatsClient::connect(server_url)?;

    // Special-case: pr recheck runs CI locally then stores results on server
    #[cfg(feature = "ci")]
    if let Commands::Pr {
        command: nusy_kanban::pr_cli::PrCommands::Recheck { id },
    } = command
    {
        return run_recheck_client(&client, id);
    }

    let (cmd, payload) = command_to_nats(command);
    let response = client.request(&cmd, &payload)?;

    // Print the response in a user-friendly way
    print_client_response(&cmd, &response);

    Ok(())
}

/// Run CI checks locally and store the results on the NATS server.
///
/// This is the server-mode implementation of `nk pr recheck`. It:
/// 1. Verifies the proposal exists on the server
/// 2. Runs cargo test/clippy/fmt locally
/// 3. Stores the results on the server via `pr.ci_store`
#[cfg(all(feature = "client", feature = "ci"))]
fn run_recheck_client(
    client: &nusy_kanban::client::NatsClient,
    proposal_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Verify proposal exists
    let _check = client.request("pr.checks", &serde_json::json!({ "id": proposal_id }))?;

    let repo_root = get_repo_root()?;
    println!("Running CI checks for {proposal_id}...\n");

    let suite = nusy_conductor::ci_runner::run_ci_checks(&repo_root);

    // Extract counts from suite
    use nusy_conductor::ci_runner::CheckType;
    let mut test_passed = 0u32;
    let mut test_failed = 0u32;
    let mut clippy_warnings = 0u32;
    let mut fmt_clean = true;

    for check in &suite.checks {
        match check.check_type {
            CheckType::Test => {
                let (p, f) = parse_test_counts_recheck(&check.summary);
                test_passed = p;
                test_failed = f;
            }
            CheckType::Clippy => {
                if !check.passed {
                    clippy_warnings = check
                        .summary
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1);
                }
            }
            CheckType::Fmt => {
                fmt_clean = check.passed;
            }
        }
    }

    let status = if suite.passed {
        "passed"
    } else if suite.error.is_some() {
        "error"
    } else {
        "failed"
    };

    let summary_text = suite.summary();
    let error_msg = suite.error.as_deref();

    // Store result on the server via pr.ci_store
    let store_payload = serde_json::json!({
        "proposal_id": proposal_id,
        "status": status,
        "test_passed": test_passed,
        "test_failed": test_failed,
        "clippy_warnings": clippy_warnings,
        "fmt_clean": fmt_clean,
        "duration_secs": suite.total_duration.as_secs_f64(),
        "error_message": error_msg,
        "summary": summary_text,
    });

    let response = client.request("pr.ci_store", &store_payload)?;
    println!("{summary_text}");

    if let Some(run_id) = response.get("run_id").and_then(|v| v.as_str()) {
        println!("\nStored as {run_id} (on server)");
    }

    Ok(())
}

/// Parse "42 passed, 3 failed" from test summary (recheck variant).
#[cfg(feature = "client")]
fn parse_test_counts_recheck(summary: &str) -> (u32, u32) {
    let passed = summary
        .split_whitespace()
        .zip(summary.split_whitespace().skip(1))
        .find(|(_, label)| *label == "passed" || label.starts_with("passed"))
        .and_then(|(num, _)| num.parse().ok())
        .unwrap_or(0);
    let failed = summary
        .split_whitespace()
        .zip(summary.split_whitespace().skip(1))
        .find(|(_, label)| *label == "failed" || label.starts_with("failed"))
        .and_then(|(num, _)| num.parse().ok())
        .unwrap_or(0);
    (passed, failed)
}

/// Get the git repo root directory.
#[cfg(feature = "client")]
fn get_repo_root() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        return Err("not inside a git repository".into());
    }
    let root = String::from_utf8(output.stdout)?;
    Ok(std::path::PathBuf::from(root.trim()))
}

/// Convert a CLI command to a NATS subject + JSON payload.
#[cfg(feature = "client")]
fn command_to_nats(command: &Commands) -> (String, serde_json::Value) {
    match command {
        Commands::Create {
            item_type,
            title,
            priority,
            assign,
            tags,
            body,
            body_file,
            body_stdin: _,
            template,
            push: _,
        } => {
            let tag_list: Vec<String> = tags
                .as_deref()
                .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();

            // For NATS mode, resolve body from --body, --body-file, or --template
            // (stdin not supported in NATS client mode)
            let body_content = if body.is_some() {
                body.clone()
            } else if let Some(path) = body_file {
                match std::fs::read_to_string(path) {
                    Ok(content) => Some(content),
                    Err(e) => {
                        eprintln!("Error: Failed to read body file '{}': {}", path, e);
                        std::process::exit(1);
                    }
                }
            } else if template.is_some() {
                Some(generate_template(
                    item_type,
                    title,
                    &std::env::current_dir().unwrap_or_default(),
                ))
            } else {
                None
            };

            (
                "create".to_string(),
                serde_json::json!({
                    "title": title,
                    "item_type": item_type,
                    "priority": priority,
                    "assignee": assign,
                    "tags": tag_list,
                    "body": body_content,
                }),
            )
        }
        Commands::Move {
            id,
            status,
            assign,
            force,
            resolution,
            closed_by,
        } => (
            "move".to_string(),
            serde_json::json!({
                "id": id,
                "status": status,
                "assignee": assign,
                "force": force,
                "resolution": resolution,
                "closed_by": closed_by,
            }),
        ),
        Commands::List {
            status,
            board,
            item_type,
            assignee,
            resolution,
            priority,
            tag,
            ready,
        } => (
            "list".to_string(),
            serde_json::json!({
                "status": status,
                "board": board,
                "item_type": item_type,
                "assignee": assignee,
                "resolution": resolution,
                "priority": priority,
                "tags": tag,
                "ready": ready,
            }),
        ),
        Commands::Board { board } => ("board".to_string(), serde_json::json!({ "board": board })),
        Commands::Show {
            id,
            format,
            relations,
        } => (
            "show".to_string(),
            serde_json::json!({ "id": id, "format": format, "relations": relations }),
        ),
        Commands::Query {
            query: words,
            search,
            sparql,
            json: _,
            verbose: _,
            no_semantic: _,
            top,
            embedding_provider,
        } => {
            let mut payload = serde_json::json!({ "query": words.join(" ") });
            if let Some(s) = search {
                payload["search"] = serde_json::json!(s);
                payload["top"] = serde_json::json!(top);
            }
            if *top != 10 {
                payload["top"] = serde_json::json!(top);
            }
            if let Some(s) = sparql {
                payload["sparql"] = serde_json::json!(s);
            }
            if let Some(ep) = embedding_provider {
                payload["embedding_provider"] = serde_json::json!(ep);
            }
            ("query".to_string(), payload)
        }
        Commands::Stats {
            board,
            velocity,
            burndown,
            by_agent,
            since,
            weeks,
        } => (
            "stats".to_string(),
            serde_json::json!({
                "board": board,
                "velocity": velocity,
                "burndown": burndown,
                "by_agent": by_agent,
                "since": since,
                "weeks": weeks,
            }),
        ),
        Commands::History {
            week,
            month,
            since,
            by_assignee,
        } => (
            "history".to_string(),
            serde_json::json!({
                "week": week,
                "month": month,
                "since": since,
                "by_assignee": by_assignee,
            }),
        ),
        Commands::Roadmap { flat, ready } => (
            "roadmap".to_string(),
            serde_json::json!({ "flat": flat, "ready": ready }),
        ),
        Commands::CriticalPath => ("critical-path".to_string(), serde_json::json!({})),
        Commands::Worklist { agents, depth } => (
            "worklist".to_string(),
            serde_json::json!({ "agents": agents, "depth": depth }),
        ),
        Commands::Blocked => ("blocked".to_string(), serde_json::json!({})),
        Commands::Validate {
            id,
            fix,
            board,
            all,
            status,
        } => (
            "validate".to_string(),
            serde_json::json!({ "id": id, "fix": fix, "board": board, "all": all, "status": status }),
        ),
        Commands::Export {
            id,
            format,
            board,
            output: _,
        } => (
            "export".to_string(),
            serde_json::json!({ "id": id, "format": format, "board": board }),
        ),
        Commands::NextId { item_type, json: _ } => (
            "next-id".to_string(),
            serde_json::json!({ "item_type": item_type }),
        ),
        Commands::Migrate { dry_run: _ } => {
            // Migration doesn't make sense in client mode
            ("stats".to_string(), serde_json::json!({}))
        }
        Commands::Hdd { command } => match &command {
            HddCommands::Run {
                experiment_id,
                agent,
            } => (
                "hdd.run".to_string(),
                serde_json::json!({ "experiment_id": experiment_id, "agent": agent }),
            ),
            HddCommands::Status { experiment_id } => (
                "hdd.run.status".to_string(),
                serde_json::json!({ "experiment_id": experiment_id }),
            ),
            HddCommands::Complete {
                experiment_id,
                run,
                results,
            } => (
                "hdd.run.complete".to_string(),
                serde_json::json!({ "experiment_id": experiment_id, "run": run, "results": results }),
            ),
            HddCommands::Paper { title, tags } => (
                "hdd.paper".to_string(),
                serde_json::json!({ "title": title, "tags": tags }),
            ),
            HddCommands::Hypothesis { title, paper, tags } => (
                "hdd.hypothesis".to_string(),
                serde_json::json!({ "title": title, "paper": paper, "tags": tags }),
            ),
            HddCommands::Experiment {
                title,
                hypothesis,
                tags,
            } => (
                "hdd.experiment".to_string(),
                serde_json::json!({ "title": title, "hypothesis": hypothesis, "tags": tags }),
            ),
            HddCommands::Measure {
                title,
                experiment,
                tags,
            } => (
                "hdd.measure".to_string(),
                serde_json::json!({ "title": title, "experiment": experiment, "tags": tags }),
            ),
            HddCommands::Idea { title, tags } => (
                "hdd.idea".to_string(),
                serde_json::json!({ "title": title, "tags": tags }),
            ),
            HddCommands::Literature { title, tags } => (
                "hdd.literature".to_string(),
                serde_json::json!({ "title": title, "tags": tags }),
            ),
            HddCommands::Validate => ("hdd.validate".to_string(), serde_json::json!({})),
            HddCommands::Registry => ("hdd.registry".to_string(), serde_json::json!({})),
        },
        Commands::Training { .. } => ("stats".to_string(), serde_json::json!({})),
        Commands::Rank { id: _, rank: _ } => ("stats".to_string(), serde_json::json!({})),
        Commands::Next { assignee } => (
            "list".to_string(),
            serde_json::json!({ "status": "backlog", "assignee": assignee }),
        ),
        Commands::Init { .. } => ("stats".to_string(), serde_json::json!({})),
        Commands::Boards => ("stats".to_string(), serde_json::json!({})),
        Commands::Templates { item_type } => (
            "templates".to_string(),
            serde_json::json!({ "item_type": item_type }),
        ),
        Commands::McpServer { .. } => ("stats".to_string(), serde_json::json!({})),
        Commands::Update {
            id,
            title,
            priority,
            assign,
            tags,
            body,
            body_file,
            related,
            depends_on,
        } => {
            let mut payload = serde_json::json!({ "id": id });
            if let Some(t) = title {
                payload["title"] = serde_json::json!(t);
            }
            if let Some(p) = priority {
                payload["priority"] = serde_json::json!(p);
            }
            if let Some(a) = assign {
                payload["assignee"] = serde_json::json!(a);
            }
            if let Some(t) = tags {
                let list: Vec<&str> = t.split(',').map(|s| s.trim()).collect();
                payload["tags"] = serde_json::json!(list);
            }
            if let Some(path) = body_file {
                if let Ok(content) = std::fs::read_to_string(path) {
                    payload["body"] = serde_json::json!(content.trim());
                }
            } else if let Some(b) = body {
                payload["body"] = serde_json::json!(b);
            }
            if let Some(r) = related {
                let list: Vec<&str> = r.split(',').map(|s| s.trim()).collect();
                payload["related"] = serde_json::json!(list);
            }
            if let Some(d) = depends_on {
                let list: Vec<&str> = d.split(',').map(|s| s.trim()).collect();
                payload["depends_on"] = serde_json::json!(list);
            }
            ("update".to_string(), payload)
        }
        Commands::Comment { id, text } => (
            "comment".to_string(),
            serde_json::json!({ "id": id, "text": text }),
        ),
        Commands::Pr { command: pr_cmd } => {
            use nusy_kanban::pr_cli::PrCommands;
            let agent = detect_agent_name();
            match pr_cmd {
                PrCommands::Create { title, base, body } => (
                    "pr.create".to_string(),
                    serde_json::json!({
                        "title": title,
                        "base": base,
                        "body": body,
                        "source_branch": get_current_git_branch(),
                        "agent_name": agent,
                    }),
                ),
                PrCommands::List => ("pr.list".to_string(), serde_json::json!({})),
                PrCommands::View { id } => ("pr.view".to_string(), serde_json::json!({ "id": id })),
                PrCommands::Diff { id } => ("pr.diff".to_string(), serde_json::json!({ "id": id })),
                PrCommands::Review {
                    id,
                    approve,
                    request_changes,
                    body,
                    reviewer,
                } => (
                    "pr.review".to_string(),
                    serde_json::json!({
                        "id": id,
                        "approve": approve,
                        "request_changes": request_changes,
                        "body": body,
                        "reviewer": reviewer.as_deref().unwrap_or(&agent),
                    }),
                ),
                PrCommands::Merge {
                    id,
                    delete_branch,
                    resolution,
                    closed_by,
                } => (
                    "pr.merge".to_string(),
                    serde_json::json!({
                        "id": id,
                        "delete_branch": delete_branch,
                        "agent_name": agent,
                        "resolution": resolution,
                        "closed_by": closed_by,
                    }),
                ),
                PrCommands::Close { id, resolution } => (
                    "pr.close".to_string(),
                    serde_json::json!({
                        "id": id,
                        "agent_name": agent,
                        "resolution": resolution,
                    }),
                ),
                PrCommands::Comment { id, body } => (
                    "pr.comment".to_string(),
                    serde_json::json!({
                        "id": id,
                        "body": body,
                        "agent_name": agent,
                    }),
                ),
                PrCommands::Checks { id } => {
                    ("pr.checks".to_string(), serde_json::json!({ "id": id }))
                }
                PrCommands::Revise { id } => (
                    "pr.revise".to_string(),
                    serde_json::json!({ "id": id, "agent_name": agent }),
                ),
                PrCommands::Resolve { id, comment_id } => (
                    "pr.resolve".to_string(),
                    serde_json::json!({ "id": id, "comment_id": comment_id }),
                ),
                PrCommands::Recheck { id } => {
                    // Recheck is handled by run_recheck_client() above.
                    // This branch is only reached in local mode (which falls through
                    // to run_pr_command). For NATS mode, the special case in
                    // run_client() intercepts it first.
                    ("pr.recheck".to_string(), serde_json::json!({ "id": id }))
                }
            }
        }
        Commands::Git { command: git_cmd } => {
            use nusy_kanban::git_cli::GitCommands;
            let agent = detect_agent_name();
            match git_cmd {
                GitCommands::Push { store } => (
                    "git.push".to_string(),
                    serde_json::json!({ "store": store, "agent_name": agent }),
                ),
                GitCommands::Pull { store } => (
                    "git.pull".to_string(),
                    serde_json::json!({ "store": store, "agent_name": agent }),
                ),
                GitCommands::Clone { store } => (
                    "git.clone".to_string(),
                    serde_json::json!({ "store": store, "agent_name": agent }),
                ),
                GitCommands::Log { limit, store } => (
                    "git.log".to_string(),
                    serde_json::json!({ "limit": limit, "store": store }),
                ),
                GitCommands::Blame { limit, store } => (
                    "git.blame".to_string(),
                    serde_json::json!({ "limit": limit, "store": store }),
                ),
                GitCommands::Rebase {
                    start,
                    end,
                    onto,
                    store,
                } => (
                    "git.rebase".to_string(),
                    serde_json::json!({
                        "start": start,
                        "end": end,
                        "onto": onto,
                        "store": store,
                        "agent_name": agent,
                    }),
                ),
            }
        }
        Commands::Source { command: src_cmd } => {
            use nusy_kanban::source_cli::SourceCommands;
            let agent = detect_agent_name();
            match src_cmd {
                SourceCommands::Push { branch, base } => {
                    let branch_name = branch.clone().unwrap_or_else(|| {
                        nusy_kanban::source_cli::current_branch()
                            .unwrap_or_else(|_| "HEAD".to_string())
                    });
                    // Create bundle locally, then send to server
                    match nusy_kanban::source_cli::create_bundle(&branch_name, base) {
                        Ok(data) => {
                            let encoded = base64_encode(&data);
                            (
                                "source.push".to_string(),
                                serde_json::json!({
                                    "branch": branch_name,
                                    "bundle_b64": encoded,
                                    "agent_name": agent,
                                    "size_bytes": data.len(),
                                }),
                            )
                        }
                        Err(e) => {
                            eprintln!("Error: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                SourceCommands::Pull { branch } => (
                    "source.pull".to_string(),
                    serde_json::json!({ "branch": branch, "agent_name": agent }),
                ),
                SourceCommands::Branches => (
                    "source.branches".to_string(),
                    serde_json::json!({ "agent_name": agent }),
                ),
                SourceCommands::Delete { branch } => (
                    "source.delete".to_string(),
                    serde_json::json!({ "branch": branch, "agent_name": agent }),
                ),
            }
        }

        // Build, Test, and Materialize are intercepted before run_client is called —
        // these arms are unreachable but required for exhaustive matching.
        #[cfg(feature = "build")]
        Commands::Build { .. } | Commands::Test { .. } => {
            unreachable!("Build/Test intercepted before NATS dispatch")
        }
        Commands::Materialize { .. } | Commands::Config(_) => {
            unreachable!("Materialize/Config intercepted before NATS dispatch")
        }
        // Backup and Restore run locally — they are intercepted before NATS dispatch.
        Commands::Backup { .. } | Commands::Restore { .. } => {
            unreachable!("Backup/Restore intercepted before NATS dispatch")
        }
    }
}

/// Base64 encode — delegates to shared `nusy_kanban::base64`.
fn base64_encode(data: &[u8]) -> String {
    nusy_kanban::base64::encode(data)
}

/// Base64 decode — delegates to shared `nusy_kanban::base64`.
fn base64_decode(input: &str) -> Vec<u8> {
    nusy_kanban::base64::decode(input)
}

/// Print a NATS response in user-friendly format.
#[cfg(feature = "client")]
fn print_client_response(command: &str, response: &serde_json::Value) {
    match command {
        "create" => {
            if let (Some(id), Some(title)) = (
                response.get("id").and_then(|v| v.as_str()),
                response.get("title").and_then(|v| v.as_str()),
            ) {
                println!("Created {id}: {title}");
            }
        }
        "move" => {
            if let (Some(id), Some(from), Some(to)) = (
                response.get("id").and_then(|v| v.as_str()),
                response.get("from").and_then(|v| v.as_str()),
                response.get("to").and_then(|v| v.as_str()),
            ) {
                println!("Moved {id} from {from} to {to}");
            }
        }
        "list" | "query" | "blocked" => {
            if let Some(table) = response.get("table").and_then(|v| v.as_str()) {
                print!("{table}");
            }
        }
        "show" => {
            if let Some(detail) = response.get("detail").and_then(|v| v.as_str()) {
                print!("{detail}");
            }
        }
        "board" | "roadmap" | "critical-path" | "worklist" => {
            if let Some(view) = response.get("view").and_then(|v| v.as_str()) {
                print!("{view}");
            }
        }
        "stats" => {
            if let Some(stats) = response.get("stats").and_then(|v| v.as_str()) {
                print!("{stats}");
            }
        }
        "history" => {
            if let Some(history) = response.get("history").and_then(|v| v.as_str()) {
                print!("{history}");
            }
        }
        "validate" => {
            if let Some(valid) = response.get("valid").and_then(|v| v.as_bool()) {
                if valid {
                    println!("Board is valid.");
                } else if let Some(issues) = response.get("issues") {
                    println!("Validation issues:\n{issues}");
                }
            }
        }
        "export" => {
            if let Some(content) = response.get("content").and_then(|v| v.as_str()) {
                print!("{content}");
            }
        }
        "next-id" => {
            if let Some(next) = response.get("next_id").and_then(|v| v.as_str()) {
                println!("{next}");
            }
        }
        "delete" => {
            if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                println!("Deleted {id}");
            }
        }
        // PR commands
        "pr.create" => {
            if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                let title = response.get("title").and_then(|v| v.as_str()).unwrap_or("");
                println!("Created proposal {id}: {title}");
                if let Some(source) = response.get("source_branch").and_then(|v| v.as_str()) {
                    let target = response
                        .get("target_branch")
                        .and_then(|v| v.as_str())
                        .unwrap_or("main");
                    println!("  {source} → {target}");
                }
            }
        }
        "pr.list" => {
            if let Some(table) = response.get("table").and_then(|v| v.as_str()) {
                print!("{table}");
            } else {
                println!("No proposals.");
            }
        }
        "pr.view" | "pr.diff" | "pr.checks" => {
            if let Some(detail) = response.get("detail").and_then(|v| v.as_str()) {
                print!("{detail}");
            }
        }
        "pr.review" => {
            if let Some(msg) = response.get("message").and_then(|v| v.as_str()) {
                println!("{msg}");
            }
        }
        "pr.merge" => {
            if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                println!("Merged {id}");
            }
        }
        "pr.close" => {
            if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                println!("Closed {id}");
            }
        }
        "pr.comment" => {
            if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                println!("Comment added to {id}");
            }
        }
        "pr.revise" => {
            if let Some(msg) = response.get("message").and_then(|v| v.as_str()) {
                println!("{msg}");
            }
        }
        "pr.resolve" => {
            if let Some(msg) = response.get("message").and_then(|v| v.as_str()) {
                println!("{msg}");
            }
        }
        // Git commands
        "git.push" | "git.pull" | "git.clone" | "git.rebase" => {
            if let Some(msg) = response.get("message").and_then(|v| v.as_str()) {
                println!("{msg}");
            }
        }
        "git.log" | "git.blame" => {
            if let Some(detail) = response.get("detail").and_then(|v| v.as_str()) {
                print!("{detail}");
            }
        }
        // Source transport
        "source.push" => {
            if let Some(msg) = response.get("message").and_then(|v| v.as_str()) {
                println!("{msg}");
            }
        }
        "source.pull" => {
            // Server returns bundle as base64 — apply it locally
            if let Some(bundle_b64) = response.get("bundle_b64").and_then(|v| v.as_str()) {
                let branch = response
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .unwrap_or("main");
                let data = base64_decode(bundle_b64);
                match nusy_kanban::source_cli::apply_bundle(&data, branch) {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => eprintln!("Error applying bundle: {e}"),
                }
            } else if let Some(msg) = response.get("message").and_then(|v| v.as_str()) {
                println!("{msg}");
            }
        }
        "source.branches" => {
            if let Some(branches) = response.get("branches").and_then(|v| v.as_array()) {
                if branches.is_empty() {
                    println!("No branches on server.");
                } else {
                    println!("Branches on server:\n");
                    for b in branches {
                        if let Some(info) = b.as_object() {
                            let name = info.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let size = info.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                            let agent = info.get("agent").and_then(|v| v.as_str()).unwrap_or("?");
                            println!("  {name}  ({size} bytes, pushed by {agent})");
                        }
                    }
                }
            }
        }
        "source.delete" => {
            if let Some(msg) = response.get("message").and_then(|v| v.as_str()) {
                println!("{msg}");
            }
        }
        "templates" => {
            if let Some(template) = response.get("template").and_then(|v| v.as_str()) {
                print!("{template}");
            } else if let Some(types) = response.get("types").and_then(|v| v.as_array()) {
                println!("Available item types:\n");
                for t in types {
                    let name = t.get("type").and_then(|v| v.as_str()).unwrap_or("-");
                    let desc = t.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    println!("  {name:12} {desc}");
                }
                println!("\nUsage: nk templates <type>");
            }
        }
        "hdd.paper" | "hdd.hypothesis" | "hdd.experiment" | "hdd.measure" | "hdd.idea"
        | "hdd.literature" => {
            if let (Some(id), Some(title)) = (
                response.get("id").and_then(|v| v.as_str()),
                response.get("title").and_then(|v| v.as_str()),
            ) {
                println!("Created {id}: {title}");
            }
        }
        "hdd.validate" | "hdd.registry" => {
            if let Some(output) = response.get("output").and_then(|v| v.as_str()) {
                print!("{output}");
            }
        }
        "hdd.run" => {
            if let Some(run_id) = response.get("run_id").and_then(|v| v.as_str()) {
                println!("Started {run_id}");
            }
        }
        "hdd.run.status" => {
            if let Some(output) = response.get("output").and_then(|v| v.as_str()) {
                print!("{output}");
            }
        }
        "hdd.run.complete" => {
            if let (Some(id), Some(run)) = (
                response.get("experiment_id").and_then(|v| v.as_str()),
                response.get("run").and_then(|v| v.as_u64()),
            ) {
                println!("Completed {id} run #{run}");
            }
        }
        _ => {
            // Generic JSON output for unknown commands
            println!(
                "{}",
                serde_json::to_string_pretty(response).unwrap_or_default()
            );
        }
    }
}

/// Detect agent name from environment or hostname.
///
/// Resolution order:
/// 1. `NUSY_AGENT_NAME` env var (explicit override)
/// 2. Hostname pattern matching (Mini, M5, DGX)
///
/// DGX hostname is `spark-*` which doesn't contain "dgx", so we match
/// that pattern explicitly.
fn detect_agent_name() -> String {
    // 1. Check env var first
    if let Ok(name) = std::env::var("NUSY_AGENT_NAME")
        && !name.is_empty()
    {
        return name;
    }

    // 2. Fall back to hostname pattern matching
    let hostname = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    if hostname.contains("mini") {
        "Mini".to_string()
    } else if hostname.contains("macbook") || hostname.contains("mac") {
        "M5".to_string()
    } else if hostname.contains("dgx") || hostname.starts_with("spark") {
        "DGX".to_string()
    } else {
        hostname
    }
}

/// Get the current git branch name (for PR source_branch in NATS mode).
#[cfg(feature = "client")]
fn get_current_git_branch() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Find items that are blocked by unmet dependencies.
///
/// Uses graph-query BFS for transitive dependency traversal with cycle detection.
/// An item is "blocked" if it has a depends_on chain where at least one
/// dependency is not in a terminal state.
fn find_blocked_items(store: &nusy_kanban::crud::KanbanStore) -> Vec<arrow::array::RecordBatch> {
    use arrow::array::{Array as _, StringArray};
    use nusy_kanban::schema::items_col;

    // Build a set of done/terminal item IDs
    let mut done_ids = std::collections::HashSet::new();
    for batch in store.items_batches() {
        let Some(ids) = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
        else {
            continue;
        };
        let Some(statuses) = batch
            .column(items_col::STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
        else {
            continue;
        };
        let Some(deleted) = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<arrow::array::BooleanArray>()
        else {
            continue;
        };
        for i in 0..batch.num_rows() {
            if !deleted.value(i) && statuses.value(i) == "done" {
                done_ids.insert(ids.value(i).to_string());
            }
        }
    }

    // Build transitive dependency graph using graph-query
    let all = store.query_items(None, None, None, None);
    let adj = build_dependency_adjacency(&all);

    // Filter: items where any transitive dependency is not done
    all.into_iter()
        .filter(|batch| {
            let Some(ids) = batch
                .column(items_col::ID)
                .as_any()
                .downcast_ref::<StringArray>()
            else {
                return false;
            };
            let Some(statuses) = batch
                .column(items_col::STATUS)
                .as_any()
                .downcast_ref::<StringArray>()
            else {
                return false;
            };
            (0..batch.num_rows()).any(|i| {
                if statuses.value(i) == "done" {
                    return false;
                }
                // Use BFS to find all transitive dependencies
                let deps = nusy_graph_query::traversal::bfs_with_adjacency(ids.value(i), &adj, 100);
                // Blocked if any transitive dep is not done
                deps.iter().any(|node| !done_ids.contains(&node.id))
            })
        })
        .collect()
}

/// Format all relationships for an item (depends_on, related, RelationsStore).
fn format_item_relations(id: &str, store: &nusy_kanban::crud::KanbanStore) -> String {
    use arrow::array::{Array as _, ListArray, StringArray};
    use nusy_kanban::schema::items_col;

    let mut lines = Vec::new();
    lines.push(String::new());
    lines.push("  Relations".to_string());
    lines.push(format!("  {}", "─".repeat(60)));

    // Get the item batch
    let Ok(item) = store.get_item(id) else {
        lines.push("  (item not found)".to_string());
        return lines.join("\n") + "\n";
    };

    // depends_on
    if let Some(deps) = item
        .column(items_col::DEPENDS_ON)
        .as_any()
        .downcast_ref::<ListArray>()
        && !deps.is_null(0)
        && !deps.value(0).is_empty()
    {
        let dep_vals = deps.value(0);
        if let Some(dep_strs) = dep_vals.as_any().downcast_ref::<StringArray>() {
            lines.push("  Depends on:".to_string());
            for j in 0..dep_strs.len() {
                if !dep_strs.is_null(j) {
                    lines.push(format!("    → {}", dep_strs.value(j)));
                }
            }
        }
    }

    // related
    if let Some(related) = item
        .column(items_col::RELATED)
        .as_any()
        .downcast_ref::<ListArray>()
        && !related.is_null(0)
        && !related.value(0).is_empty()
    {
        let rel_vals = related.value(0);
        if let Some(rel_strs) = rel_vals.as_any().downcast_ref::<StringArray>() {
            lines.push("  Related:".to_string());
            for j in 0..rel_strs.len() {
                if !rel_strs.is_null(j) {
                    lines.push(format!("    ↔ {}", rel_strs.value(j)));
                }
            }
        }
    }

    // Transitive dependency chain (via graph-query BFS)
    let all = store.query_items(None, None, None, None);
    let adj = build_dependency_adjacency(&all);

    let chain = nusy_graph_query::traversal::bfs_with_adjacency(id, &adj, 10);
    if !chain.is_empty() {
        lines.push("  Dependency chain (transitive):".to_string());
        for node in &chain {
            let indent = "  ".repeat(node.depth);
            lines.push(format!("    {indent}↳ {} (depth {})", node.id, node.depth));
        }
    }

    if lines.len() <= 3 {
        lines.push("  (no relationships)".to_string());
    }

    lines.join("\n") + "\n"
}

/// Build dependency adjacency map from all items.
fn build_dependency_adjacency(
    all_items: &[arrow::array::RecordBatch],
) -> std::collections::HashMap<String, Vec<String>> {
    use nusy_kanban::schema::items_col;

    let mut adj = std::collections::HashMap::new();
    for batch in all_items {
        let partial = nusy_graph_query::traversal::build_adjacency_from_list(
            batch,
            items_col::ID,
            items_col::DEPENDS_ON,
            nusy_graph_query::traversal::Direction::Forward,
        );
        for (k, v) in partial {
            adj.entry(k).or_insert_with(Vec::new).extend(v);
        }
    }
    adj
}

/// Build a map of item ID → status for chain display.
fn build_status_map(
    store: &nusy_kanban::crud::KanbanStore,
) -> std::collections::HashMap<String, String> {
    use arrow::array::StringArray;
    use nusy_kanban::schema::items_col;

    let mut map = std::collections::HashMap::new();
    for batch in store.items_batches() {
        let Some(ids) = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
        else {
            continue;
        };
        let Some(statuses) = batch
            .column(items_col::STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
        else {
            continue;
        };
        for i in 0..batch.num_rows() {
            map.insert(ids.value(i).to_string(), statuses.value(i).to_string());
        }
    }
    map
}
