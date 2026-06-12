//! Request-reply handlers for all kanban commands.
//!
//! Each `kanban.cmd.{command}` subject maps to a handler function that
//! deserializes the JSON payload, calls the corresponding `nusy-kanban`
//! library function, and returns a JSON response.
//!
//! Modules:
//! - [`core`] — CRUD, analytics, HDD creation
//! - [`relations`] — Dependency graph
//! - [`pr`] — PR / Proposal lifecycle (feature: `pr`)
//! - [`source`] — Git bundle transport (feature: `git`)
//! - [`research`] — HDD experiment run tracking (feature: `research`)

pub mod core;
#[cfg(feature = "pr")]
pub mod pr;
pub mod relations;
#[cfg(feature = "research")]
pub mod research;
#[cfg(feature = "git")]
pub mod source;

use core::*;
#[cfg(feature = "pr")]
use pr::*;
use relations::*;
#[cfg(feature = "research")]
use research::*;
#[cfg(feature = "git")]
use source::*;

use crate::state::ServerState;
use nusy_kanban::item_type::ItemType;
use serde::Serialize;

/// Unified error response sent back to NATS clients.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: &'static str,
}

/// Default board states (matching yurtle-kanban config).
pub(crate) const DEFAULT_STATES: &[&str] = &["backlog", "in_progress", "review", "done"];

/// Dispatch a NATS message to the appropriate handler based on subject.
///
/// Returns the JSON-serialized response bytes.
pub fn dispatch(subject: &str, payload: &[u8], state: &mut ServerState) -> Vec<u8> {
    let command = subject.strip_prefix("kanban.cmd.").unwrap_or(subject);

    let result = match command {
        // Core CRUD + analytics
        "create" => handle_create(payload, &mut state.store),
        "move" => handle_move(payload, &mut state.store),
        "update" => handle_update(payload, &mut state.store),
        "comment" => handle_comment(payload, &mut state.store),
        "rank" => handle_rank(payload, &mut state.store),
        "list" => handle_list(payload, &state.store),
        "show" => handle_show(payload, &state.store),
        "query" => handle_query(payload, &state.store, &state.proposals),
        "board" => handle_board(payload, &state.store),
        "stats" => handle_stats(payload, &state.store),
        "delete" => handle_delete(payload, &mut state.store),
        "validate" => handle_validate(&state.store, &state.relations),
        "export" => handle_export(payload, &state.store),
        "roadmap" => handle_roadmap(payload, &state.store),
        "critical-path" => handle_critical_path(&state.store),
        "worklist" => handle_worklist(payload, &state.store),
        "next-id" => handle_next_id(payload, &state.store),
        "history" => handle_history(payload, &state.store),
        "blocked" => handle_blocked(&state.store),
        "templates" => handle_templates(payload, &state.data_dir),
        // HDD create commands
        "hdd.paper" => handle_hdd_create(
            payload,
            &mut state.store,
            &mut state.relations,
            ItemType::Paper,
        ),
        "hdd.hypothesis" => handle_hdd_create(
            payload,
            &mut state.store,
            &mut state.relations,
            ItemType::Hypothesis,
        ),
        "hdd.experiment" => handle_hdd_create(
            payload,
            &mut state.store,
            &mut state.relations,
            ItemType::Experiment,
        ),
        "hdd.measure" => handle_hdd_create(
            payload,
            &mut state.store,
            &mut state.relations,
            ItemType::Measure,
        ),
        "hdd.idea" => handle_hdd_create(
            payload,
            &mut state.store,
            &mut state.relations,
            ItemType::Idea,
        ),
        "hdd.literature" => handle_hdd_create(
            payload,
            &mut state.store,
            &mut state.relations,
            ItemType::Literature,
        ),
        "hdd.validate" => handle_hdd_validate(&state.store, &state.relations),
        "hdd.registry" => handle_hdd_registry(&state.store, &state.relations),
        // Relations
        "relation.add" => handle_relation_add(payload, &mut state.relations),
        "relation.query" => handle_relation_query(payload, &state.relations),
        // PR / Proposal commands (feature-gated)
        #[cfg(feature = "pr")]
        "pr.create" => handle_pr_create(payload, &mut state.proposals),
        #[cfg(feature = "pr")]
        "pr.list" => handle_pr_list(payload, &state.proposals),
        #[cfg(feature = "pr")]
        "pr.view" => handle_pr_view(payload, &state.proposals, &state.comments),
        #[cfg(feature = "pr")]
        "pr.diff" => handle_pr_diff(payload, &state.proposals),
        #[cfg(feature = "pr")]
        "pr.review" => handle_pr_review(payload, &mut state.proposals, &mut state.comments),
        #[cfg(feature = "pr")]
        "pr.merge" => handle_pr_merge(payload, &mut state.proposals),
        #[cfg(feature = "pr")]
        "pr.close" => handle_pr_close(payload, &mut state.proposals),
        #[cfg(feature = "pr")]
        "pr.comment" => handle_pr_comment(payload, &state.proposals, &mut state.comments),
        #[cfg(feature = "pr")]
        "pr.checks" => handle_pr_checks(payload, &state.proposals, &state.ci_results),
        #[cfg(feature = "pr")]
        "pr.revise" => handle_pr_revise(payload, &mut state.proposals),
        #[cfg(feature = "pr")]
        "pr.resolve" => handle_pr_resolve(payload, &mut state.proposals, &mut state.comments),
        #[cfg(feature = "pr")]
        "pr.ci_store" => handle_pr_ci_store(payload, &mut state.ci_results),
        // Git commands — graph-native versioning
        "git.push" | "git.pull" | "git.clone" => serialize_response(&serde_json::json!({
            "message": format!(
                "git.{} acknowledged. Graph git stores are currently per-agent \
                 (local .nusy-arrow/ directories). Server-managed git state is \
                 planned for VY-3009 Phase 2.",
                command.strip_prefix("git.").unwrap_or(command)
            ),
        })),
        "git.log" | "git.blame" | "git.rebase" => serialize_response(&serde_json::json!({
            "detail": format!(
                "git.{}: operates on local graph store. Use --store to specify path. \
                 Server-side git operations planned for VY-3009 Phase 2.",
                command.strip_prefix("git.").unwrap_or(command)
            ),
        })),
        // HDD experiment run tracking (feature-gated)
        #[cfg(feature = "research")]
        "hdd.run" => handle_hdd_run(payload, &mut state.store, &state.data_dir),
        #[cfg(feature = "research")]
        "hdd.run.status" => handle_hdd_run_status(payload, &state.data_dir),
        #[cfg(feature = "research")]
        "hdd.run.complete" => handle_hdd_run_complete(payload, &mut state.store, &state.data_dir),
        // Source code transport (feature-gated)
        #[cfg(feature = "git")]
        "source.push" => handle_source_push(payload, &state.data_dir),
        #[cfg(feature = "git")]
        "source.pull" => handle_source_pull(payload, &state.data_dir),
        #[cfg(feature = "git")]
        "source.branches" => handle_source_branches(&state.data_dir),
        #[cfg(feature = "git")]
        "source.delete" => handle_source_delete(payload, &state.data_dir),
        _ => Err(error_response(
            &format!("Unknown command: {command}"),
            "UNKNOWN_COMMAND",
        )),
    };

    // After successful mutations, persist state
    if result.is_ok() && is_mutation(command) {
        if let Err(e) = nusy_kanban::persist::save_store(&state.data_dir, &state.store) {
            eprintln!("Warning: failed to persist store after {command}: {e}");
        }
        if is_relation_mutation(command)
            && let Err(e) = nusy_kanban::persist::save_relations(&state.data_dir, &state.relations)
        {
            eprintln!("Warning: failed to persist relations after {command}: {e}");
        }
        #[cfg(feature = "pr")]
        if is_pr_mutation(command)
            && let Err(e) = nusy_kanban::persist::save_proposals(
                &state.data_dir,
                &state.proposals,
                &state.comments,
                &state.ci_results,
            )
        {
            eprintln!("Warning: failed to persist proposals after {command}: {e}");
        }
    }

    match result {
        Ok(bytes) => bytes,
        Err(bytes) => bytes,
    }
}

fn is_mutation(command: &str) -> bool {
    matches!(
        command,
        "create"
            | "move"
            | "update"
            | "comment"
            | "delete"
            | "rank"
            | "hdd.paper"
            | "hdd.hypothesis"
            | "hdd.experiment"
            | "hdd.measure"
            | "hdd.idea"
            | "hdd.literature"
            | "relation.add"
            | "pr.create"
            | "pr.review"
            | "pr.merge"
            | "pr.close"
            | "pr.comment"
            | "pr.revise"
            | "pr.resolve"
            | "hdd.run"
            | "hdd.run.complete"
    )
}

fn is_relation_mutation(command: &str) -> bool {
    command == "relation.add"
}

#[cfg(feature = "pr")]
fn is_pr_mutation(command: &str) -> bool {
    matches!(
        command,
        "pr.create"
            | "pr.review"
            | "pr.merge"
            | "pr.close"
            | "pr.comment"
            | "pr.revise"
            | "pr.resolve"
            | "pr.ci_store"
    )
}

pub(crate) fn error_response(msg: &str, code: &'static str) -> Vec<u8> {
    serde_json::to_vec(&ErrorResponse {
        error: msg.to_string(),
        code,
    })
    .unwrap_or_else(|_| br#"{"error":"serialization failed","code":"INTERNAL"}"#.to_vec())
}

pub(crate) fn parse_payload<T: for<'de> serde::Deserialize<'de>>(
    payload: &[u8],
) -> Result<T, Vec<u8>> {
    serde_json::from_slice(payload)
        .map_err(|e| error_response(&format!("Invalid JSON: {e}"), "INVALID_PAYLOAD"))
}

pub(crate) fn serialize_response<T: Serialize>(value: &T) -> Result<Vec<u8>, Vec<u8>> {
    serde_json::to_vec(value)
        .map_err(|e| error_response(&format!("Serialization error: {e}"), "INTERNAL"))
}

pub(crate) fn states_as_strings() -> Vec<String> {
    DEFAULT_STATES.iter().map(|s| s.to_string()).collect()
}
