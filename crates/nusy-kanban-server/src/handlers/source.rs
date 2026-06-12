//! Git bundle transport handlers.

use super::{error_response, parse_payload, serialize_response};
use serde::Deserialize;

pub(crate) fn handle_source_push(
    payload: &[u8],
    root: &std::path::Path,
) -> Result<Vec<u8>, Vec<u8>> {
    #[derive(Deserialize)]
    struct Req {
        branch: String,
        bundle_b64: String,
        agent_name: Option<String>,
    }
    let req: Req = parse_payload(payload)?;

    let bundles_dir = root.join("bundles");
    std::fs::create_dir_all(&bundles_dir)
        .map_err(|e| error_response(&format!("create bundles dir: {e}"), "SOURCE_PUSH_FAILED"))?;

    let safe_name = req.branch.replace('/', "_");
    let bundle_path = bundles_dir.join(format!("{safe_name}.bundle"));

    // Decode base64
    let data = base64_decode_simple(&req.bundle_b64);
    std::fs::write(&bundle_path, &data)
        .map_err(|e| error_response(&format!("write bundle: {e}"), "SOURCE_PUSH_FAILED"))?;

    // Write metadata
    let meta_path = bundles_dir.join(format!("{safe_name}.meta.json"));
    let meta = serde_json::json!({
        "branch": req.branch,
        "agent": req.agent_name.as_deref().unwrap_or("unknown"),
        "size_bytes": data.len(),
        "pushed_at": chrono::Utc::now().to_rfc3339(),
    });
    let _ = std::fs::write(
        &meta_path,
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    );

    let agent = req.agent_name.as_deref().unwrap_or("agent");
    serialize_response(&serde_json::json!({
        "message": format!("Pushed {} ({} bytes) by {}", req.branch, data.len(), agent),
        "branch": req.branch,
        "size_bytes": data.len(),
    }))
}

pub(crate) fn handle_source_pull(
    payload: &[u8],
    root: &std::path::Path,
) -> Result<Vec<u8>, Vec<u8>> {
    #[derive(Deserialize)]
    struct Req {
        branch: String,
    }
    let req: Req = parse_payload(payload)?;

    let safe_name = req.branch.replace('/', "_");
    let bundle_path = root.join("bundles").join(format!("{safe_name}.bundle"));

    if !bundle_path.exists() {
        return Err(error_response(
            &format!("no bundle for branch '{}' on server", req.branch),
            "SOURCE_NOT_FOUND",
        ));
    }

    let data = std::fs::read(&bundle_path)
        .map_err(|e| error_response(&format!("read bundle: {e}"), "SOURCE_PULL_FAILED"))?;

    let encoded = base64_encode_simple(&data);

    serialize_response(&serde_json::json!({
        "branch": req.branch,
        "bundle_b64": encoded,
        "size_bytes": data.len(),
    }))
}

pub(crate) fn handle_source_branches(root: &std::path::Path) -> Result<Vec<u8>, Vec<u8>> {
    let bundles_dir = root.join("bundles");
    if !bundles_dir.exists() {
        return serialize_response(&serde_json::json!({ "branches": [] }));
    }

    let mut branches = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&bundles_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".bundle") {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);

                // Read branch name + agent from metadata (avoids lossy filename→branch recovery)
                let meta_path = bundles_dir.join(name.replace(".bundle", ".meta.json"));
                let meta = std::fs::read_to_string(&meta_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
                let branch_name = meta
                    .as_ref()
                    .and_then(|v| v.get("branch").and_then(|b| b.as_str()).map(String::from))
                    .unwrap_or_else(|| name.strip_suffix(".bundle").unwrap_or(&name).to_string());
                let agent = meta
                    .as_ref()
                    .and_then(|v| v.get("agent").and_then(|a| a.as_str()).map(String::from))
                    .unwrap_or_else(|| "unknown".to_string());

                branches.push(serde_json::json!({
                    "name": branch_name,
                    "size_bytes": size,
                    "agent": agent,
                }));
            }
        }
    }

    branches.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .cmp(&b.get("name").and_then(|v| v.as_str()))
    });

    serialize_response(&serde_json::json!({ "branches": branches }))
}

pub(crate) fn handle_source_delete(
    payload: &[u8],
    root: &std::path::Path,
) -> Result<Vec<u8>, Vec<u8>> {
    #[derive(Deserialize)]
    struct Req {
        branch: String,
    }
    let req: Req = parse_payload(payload)?;

    let safe_name = req.branch.replace('/', "_");
    let bundle_path = root.join("bundles").join(format!("{safe_name}.bundle"));
    let meta_path = root.join("bundles").join(format!("{safe_name}.meta.json"));

    if !bundle_path.exists() {
        return Err(error_response(
            &format!("no bundle for branch '{}'", req.branch),
            "SOURCE_NOT_FOUND",
        ));
    }

    let _ = std::fs::remove_file(&bundle_path);
    let _ = std::fs::remove_file(&meta_path);

    serialize_response(&serde_json::json!({
        "message": format!("Deleted bundle for '{}'", req.branch),
        "branch": req.branch,
    }))
}

/// Delegates to shared `nusy_kanban::base64`.
pub(crate) fn base64_encode_simple(data: &[u8]) -> String {
    nusy_kanban::base64::encode(data)
}

/// Delegates to shared `nusy_kanban::base64`.
pub(crate) fn base64_decode_simple(input: &str) -> Vec<u8> {
    nusy_kanban::base64::decode(input)
}
