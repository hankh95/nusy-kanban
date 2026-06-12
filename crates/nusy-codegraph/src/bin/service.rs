//! nusy-codegraph-service — Code graph NATS service.
//!
//! Starts a NATS service on `codegraph.cmd.*` that exposes 8 code graph tools.
//! Loads the code graph from Parquet on startup and persists mutations back.
//!
//! EX-3184: With `--sync`, publishes code_replace updates to NATS for cross-agent
//! synchronization and subscribes to updates from other agents.
//!
//! # Usage
//!
//! ```bash
//! nusy-codegraph-service --graph-dir .codegraph --workspace . --nats-url nats://192.168.8.110:4222
//! ```

use clap::Parser;
use noesis_ship::service::NatsServiceBuilder;
use nusy_codegraph::nats_service::{
    CodeGraphState, handle_build, handle_deps, handle_query, handle_read, handle_replace,
    handle_search, handle_test, handle_tools,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "nusy-codegraph-service", about = "Code graph NATS service")]
struct Args {
    /// Directory containing nodes.parquet and edges.parquet.
    #[arg(long, default_value = "research/shared")]
    graph_dir: PathBuf,

    /// Workspace root for cargo build/test commands.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// NATS server URL.
    #[arg(long, default_value = "nats://localhost:4222")]
    nats_url: String,

    /// Explicit path to cargo binary. If omitted, resolved automatically
    /// (checks ~/.cargo/bin/cargo, then PATH). Required for launchd services
    /// where PATH doesn't include ~/.cargo/bin.
    #[arg(long)]
    cargo_path: Option<PathBuf>,

    /// Agent name for NATS sync (used to skip own updates). Defaults to hostname.
    #[arg(long, default_value_t = default_agent_name())]
    agent_name: String,

    /// Enable cross-agent NATS sync for code_replace mutations.
    #[arg(long)]
    sync: bool,
}

fn default_agent_name() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

#[tokio::main]
async fn main() -> noesis_ship::types::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nusy_codegraph=info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();
    let mut state =
        CodeGraphState::load(&args.graph_dir, &args.workspace, args.cargo_path.as_deref());

    // EX-3184: Set up NATS sync if enabled.
    if args.sync {
        match nusy_codegraph::nats_sync::CodeGraphPublisher::new(&args.nats_url, &args.agent_name)
            .await
        {
            Ok(publisher) => {
                tracing::info!(agent = %args.agent_name, "NATS graph sync enabled");
                state.sync_publisher = Some(publisher);
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to connect NATS sync publisher — running without sync");
            }
        }
    }

    // EX-3184: Spawn background subscriber for incoming graph updates.
    if args.sync {
        let sync_graph = std::sync::Arc::new(tokio::sync::Mutex::new(
            nusy_codegraph::nats_sync::SyncableGraph {
                nodes: state.nodes.clone(),
                edges: state.edges.clone(),
                body_hashes: std::collections::HashMap::new(),
            },
        ));
        let nats_url = args.nats_url.clone();
        let agent = args.agent_name.clone();
        tokio::spawn(async move {
            nusy_codegraph::nats_sync::subscribe_and_apply(&nats_url, sync_graph, &agent).await;
        });
    }

    tracing::info!(
        nodes = state.nodes.num_rows(),
        edges = state.edges.num_rows(),
        graph_dir = %args.graph_dir.display(),
        cargo = %state.cargo_path.display(),
        sync = args.sync,
        "codegraph service starting"
    );

    NatsServiceBuilder::new("codegraph.cmd", state)
        .nats_url(&args.nats_url)
        .handler("search", handle_search)
        .handler("read", handle_read)
        .handler("dependencies", handle_deps)
        .handler("replace", handle_replace)
        .handler("query", handle_query)
        .handler("build", handle_build)
        .handler("test", handle_test)
        .handler("tools", handle_tools)
        .run()
        .await
}
