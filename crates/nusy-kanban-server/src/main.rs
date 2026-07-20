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
    // CH-6055 / HZ-6053: an interrupted save leaves a WAL and/or orphaned
    // `*.parquet.tmp`. Quarantine that evidence BEFORE loading — the next save
    // would otherwise overwrite it, and during HZ-6053 the interrupted write
    // was the only copy of two months of state. Never delete it.
    match nusy_kanban::persistence::PersistenceEngine::recover_wal(data_dir) {
        Ok(rec) if rec.recovered => {
            eprintln!(
                "kanban: recovered from an INTERRUPTED SAVE — {} file(s) quarantined to {}",
                rec.quarantined.len(),
                rec.quarantine_dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<none>".into())
            );
            if rec.is_suspect() {
                eprintln!(
                    "kanban: 🔴 SUSPECT LOAD — the interrupted save is NEWER than the state about \
                     to be loaded ({}). Serving this is probably a ROLLBACK. Inspect the \
                     quarantine before trusting the board; see HZ-6053.",
                    rec.suspect
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        Ok(_) => {}
        Err(e) => eprintln!("kanban: WAL recovery check failed: {e}"),
    }

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

    // CH-6056: probe the store before serving. Coming up "ready" on a store
    // that cannot take a write is how HZ-6053 stayed invisible for days.
    let mut health = nusy_kanban_server::health::HealthGate::new();
    health.probe_now(&nusy_kanban_server::health::store_dir(data_dir));
    if health.is_degraded() {
        eprintln!(
            "kanban: 🔴 STARTING DEGRADED — {}. Serving READS ONLY; mutations are refused until \
             the store accepts writes. See HZ-6053.",
            health.reason().unwrap_or("store is not writable")
        );
    }

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
        health,
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
