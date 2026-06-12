//! nusy-kanban-server — NATS server for the Arrow-native kanban engine.
//!
//! Uses noesis-ship NatsServiceBuilder for NATS lifecycle and JetStream
//! EventBus for durable mutation events.

use clap::Parser;
use noesis_ship::service::NatsServiceBuilder;
use noesis_ship::types::StreamConfig;
use nusy_kanban::backup::{self, BackupConfig};
use nusy_kanban::persist;
use nusy_kanban_server::events::detect_mutation;
use nusy_kanban_server::handlers;
use nusy_kanban_server::state::ServerState;

fn main() {
    let args = noesis_ship::service::ServiceArgs::parse();
    let state = load_state(&args.data_dir);

    // Run startup backup check in a background thread so it doesn't delay server start.
    let backup_root = args.data_dir.clone();
    std::thread::spawn(move || {
        let config = BackupConfig::default();
        match backup::is_backup_due(&config) {
            Ok(true) => {
                eprintln!(
                    "[backup] Snapshot due, creating backup to {:?} ...",
                    config.destination
                );
                match backup::create_snapshot(&config, &backup_root) {
                    Ok(path) => {
                        eprintln!(
                            "[backup] Snapshot created: {}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                    Err(e) => {
                        eprintln!("[backup] Warning: failed to create snapshot: {e}");
                    }
                }
            }
            Ok(false) => {
                eprintln!("[backup] No backup due.");
            }
            Err(e) => {
                eprintln!("[backup] Warning: could not determine backup status: {e}");
            }
        }
    });

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(run(args, state)) {
        eprintln!("Server error: {e}");
        std::process::exit(1);
    }
}

async fn run(
    args: noesis_ship::service::ServiceArgs,
    state: ServerState,
) -> noesis_ship::types::Result<()> {
    let kanban_events = StreamConfig::new("KANBAN_EVENTS", vec!["kanban.event.>".to_string()]);

    NatsServiceBuilder::new("kanban.cmd", state)
        .nats_url(&args.nats_url)
        .default_handler(handlers::dispatch)
        .mutation_callback(|command, response, _state| detect_mutation(command, response))
        .event_prefix("kanban.event")
        .event_bus_stream(kanban_events, "kanban-server")
        .on_shutdown(persist_state)
        .run()
        .await
}

fn load_state(data_dir: &std::path::Path) -> ServerState {
    let store = persist::load_store(data_dir).unwrap_or_else(|e| {
        eprintln!("Failed to load kanban state from {data_dir:?}: {e}");
        std::process::exit(1);
    });
    let relations = persist::load_relations(data_dir).unwrap_or_else(|e| {
        eprintln!("Failed to load relations from {data_dir:?}: {e}");
        std::process::exit(1);
    });
    #[cfg(feature = "pr")]
    let (proposals, comments, ci_results) = persist::load_proposals(data_dir).unwrap_or_else(|e| {
        eprintln!("Warning: failed to load proposals: {e}");
        (
            nusy_graph_review::ProposalStore::new(),
            nusy_graph_review::CommentStore::new(),
            nusy_graph_review::CiResultStore::new(),
        )
    });

    ServerState {
        store,
        relations,
        #[cfg(feature = "pr")]
        proposals,
        #[cfg(feature = "pr")]
        comments,
        #[cfg(feature = "pr")]
        ci_results,
        data_dir: data_dir.to_path_buf(),
    }
}

fn persist_state(state: &ServerState) {
    if let Err(e) = persist::save_store(&state.data_dir, &state.store) {
        eprintln!("Warning: failed to save store: {e}");
    }
    if let Err(e) = persist::save_relations(&state.data_dir, &state.relations) {
        eprintln!("Warning: failed to save relations: {e}");
    }
    #[cfg(feature = "pr")]
    if let Err(e) = persist::save_proposals(
        &state.data_dir,
        &state.proposals,
        &state.comments,
        &state.ci_results,
    ) {
        eprintln!("Warning: failed to save proposals: {e}");
    }
}
