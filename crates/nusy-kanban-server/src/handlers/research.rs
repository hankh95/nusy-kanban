//! HDD experiment run tracking handlers.

use super::{error_response, serialize_response};

pub(crate) fn handle_hdd_run(
    payload: &[u8],
    store: &mut nusy_kanban::KanbanStore,
    root: &std::path::Path,
) -> Result<Vec<u8>, Vec<u8>> {
    let params: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| error_response(&e.to_string(), "INVALID_JSON"))?;

    let experiment_id = params
        .get("experiment_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| error_response("Missing experiment_id", "MISSING_PARAM"))?;
    let agent = params.get("agent").and_then(|v| v.as_str());

    // Verify the experiment exists
    store.get_item(experiment_id).map_err(|_| {
        error_response(
            &format!("Experiment not found: {experiment_id}"),
            "NOT_FOUND",
        )
    })?;

    let mut run_store = nusy_kanban::persist::load_experiment_runs(root);
    let run_id = run_store
        .start_run(experiment_id, agent)
        .map_err(|e| error_response(&e.to_string(), "RUN_ERROR"))?;
    nusy_kanban::persist::save_experiment_runs(root, &run_store)
        .map_err(|e| error_response(&e.to_string(), "PERSIST_ERROR"))?;

    serialize_response(&serde_json::json!({
        "run_id": run_id,
        "experiment_id": experiment_id,
        "status": "running"
    }))
}

pub(crate) fn handle_hdd_run_status(
    payload: &[u8],
    root: &std::path::Path,
) -> Result<Vec<u8>, Vec<u8>> {
    let params: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| error_response(&e.to_string(), "INVALID_JSON"))?;

    let experiment_id = params
        .get("experiment_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| error_response("Missing experiment_id", "MISSING_PARAM"))?;

    let run_store = nusy_kanban::persist::load_experiment_runs(root);
    let runs = run_store.list_runs(experiment_id);
    let output = nusy_kanban::experiment_runs::format_runs(&runs);

    serialize_response(&serde_json::json!({
        "experiment_id": experiment_id,
        "runs": runs.len(),
        "output": output
    }))
}

pub(crate) fn handle_hdd_run_complete(
    payload: &[u8],
    store: &mut nusy_kanban::KanbanStore,
    root: &std::path::Path,
) -> Result<Vec<u8>, Vec<u8>> {
    let params: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| error_response(&e.to_string(), "INVALID_JSON"))?;

    let experiment_id = params
        .get("experiment_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| error_response("Missing experiment_id", "MISSING_PARAM"))?;
    let run_number = params
        .get("run")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .ok_or_else(|| error_response("Missing run number", "MISSING_PARAM"))?;
    let results = params.get("results").and_then(|v| v.as_str());

    // Verify experiment exists
    store.get_item(experiment_id).map_err(|_| {
        error_response(
            &format!("Experiment not found: {experiment_id}"),
            "NOT_FOUND",
        )
    })?;

    let mut run_store = nusy_kanban::persist::load_experiment_runs(root);
    run_store
        .complete_run(experiment_id, run_number, results)
        .map_err(|e| error_response(&e.to_string(), "RUN_ERROR"))?;
    nusy_kanban::persist::save_experiment_runs(root, &run_store)
        .map_err(|e| error_response(&e.to_string(), "PERSIST_ERROR"))?;

    serialize_response(&serde_json::json!({
        "experiment_id": experiment_id,
        "run": run_number,
        "status": "complete"
    }))
}
