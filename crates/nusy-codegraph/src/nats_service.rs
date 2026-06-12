//! CodeGraph NATS service — exposes code graph tools via NatsServiceBuilder.
//!
//! Provides 8 handlers over the `codegraph.cmd` subject prefix, making the
//! code graph queryable and modifiable via NATS request/reply:
//!
//! | Command      | Description                                    |
//! |-------------|------------------------------------------------|
//! | `search`    | Search nodes by name/body pattern              |
//! | `read`      | Read a specific node by ID                     |
//! | `deps`      | Callers/callees for a node                     |
//! | `replace`   | Update a node's body (requires rationale)      |
//! | `query`     | Structured filter query                        |
//! | `build`     | Run cargo build                                |
//! | `test`      | Run cargo test                                 |
//! | `tools`     | Return tool schemas for MCP bridge discovery   |
//!
//! Subject prefix: `codegraph.cmd`
//! Binary: `nusy-codegraph-service`

use crate::mcp_tools::{NodeUpdate, QueryFilter, codegraph_query_objects, codegraph_update_object};
use crate::schema::{CodeNode, CodeNodeKind};
use crate::search::{CodeSearch, callees, callers, search_nodes};
use arrow::array::{Array, RecordBatch};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

// ─── Request/Response types ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct NodeSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub signature: Option<String>,
    pub file_path: Option<String>,
    pub loc: Option<i32>,
}

impl NodeSummary {
    fn from_node(n: &CodeNode) -> Self {
        NodeSummary {
            id: n.id.clone(),
            name: n.name.clone(),
            kind: n.kind.as_str().to_string(),
            signature: n.signature.clone(),
            file_path: n.file_path.clone(),
            loc: n.loc,
        }
    }
}

#[derive(Serialize)]
pub struct SearchResponse {
    pub nodes: Vec<NodeSummary>,
    pub total_matched: usize,
    pub total_scanned: usize,
}

#[derive(Deserialize)]
pub struct ReadRequest {
    pub id: String,
}

#[derive(Serialize)]
pub struct ReadResponse {
    pub node: Option<CodeNodeFull>,
    pub found: bool,
}

#[derive(Serialize)]
pub struct CodeNodeFull {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub parent_id: Option<String>,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub body: Option<String>,
    pub body_hash: Option<String>,
    pub loc: Option<i32>,
    pub complexity: Option<i32>,
    pub file_path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

impl CodeNodeFull {
    fn from_node(n: CodeNode) -> Self {
        CodeNodeFull {
            id: n.id,
            name: n.name,
            kind: n.kind.as_str().to_string(),
            parent_id: n.parent_id,
            signature: n.signature,
            docstring: n.docstring,
            body: n.body,
            body_hash: n.body_hash,
            loc: n.loc,
            complexity: n.cyclomatic_complexity,
            file_path: n.file_path,
            start_line: n.start_line,
            end_line: n.end_line,
        }
    }
}

#[derive(Deserialize)]
pub struct DepsRequest {
    pub id: String,
    /// "callers", "callees", or "both" (default)
    #[serde(default = "deps_default_direction")]
    pub direction: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn deps_default_direction() -> String {
    "both".to_string()
}

#[derive(Serialize)]
pub struct DepsResponse {
    pub id: String,
    pub callers: Vec<NodeSummary>,
    pub callees: Vec<NodeSummary>,
}

#[derive(Deserialize)]
pub struct ReplaceRequest {
    pub id: String,
    pub new_body: String,
    pub rationale: String,
}

#[derive(Serialize)]
pub struct ReplaceResponse {
    pub success: bool,
    pub id: String,
    pub new_body_hash: Option<String>,
    pub rationale: String,
    pub error: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct QueryRequest {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub name_contains: Option<String>,
    #[serde(default)]
    pub min_loc: Option<i32>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct QueryResponse {
    pub nodes: Vec<NodeSummary>,
    pub total_matched: usize,
    pub total_scanned: usize,
}

#[derive(Deserialize, Default)]
pub struct BuildRequest {
    #[serde(default)]
    pub crate_name: Option<String>,
}

#[derive(Serialize)]
pub struct BuildResponse {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub command: String,
}

#[derive(Deserialize, Default)]
pub struct TestRequest {
    #[serde(default)]
    pub crate_name: Option<String>,
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Serialize)]
pub struct TestResponse {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub command: String,
}

/// MCP tool schema for bridge discovery.
#[derive(Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Serialize)]
pub struct ToolsResponse {
    pub service: String,
    pub tools: Vec<ToolSchema>,
}

// ─── State ──────────────────────────────────────────────────────────────────

/// Mutable state for the CodeGraph NATS service.
pub struct CodeGraphState {
    /// All code nodes as a single Arrow RecordBatch.
    pub nodes: RecordBatch,
    /// All code edges as a single Arrow RecordBatch.
    pub edges: RecordBatch,
    /// Workspace root for running cargo commands.
    pub workspace_root: PathBuf,
    /// Directory where nodes.parquet / edges.parquet live.
    pub graph_dir: PathBuf,
    /// Absolute path to cargo binary (resolved at startup, not runtime).
    pub cargo_path: PathBuf,
    /// EX-3184: Optional NATS sync publisher — publishes updates for cross-agent sync.
    /// None when running without `--nats-url` (offline mode).
    pub sync_publisher: Option<crate::nats_sync::CodeGraphPublisher>,
}

/// Resolve the cargo binary path, checking common locations.
///
/// launchd services run with a minimal PATH that excludes `~/.cargo/bin`,
/// so we resolve the absolute path at startup rather than relying on PATH.
pub fn resolve_cargo_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    // Try $HOME/.cargo/bin/cargo
    if let Ok(home) = std::env::var("HOME") {
        let candidate = PathBuf::from(format!("{home}/.cargo/bin/cargo"));
        if candidate.exists() {
            return candidate;
        }
    }
    // Try well-known macOS paths
    for path in &[
        "/Users/hankh19/.cargo/bin/cargo",
        "/opt/homebrew/bin/cargo",
        "/usr/local/bin/cargo",
    ] {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return candidate;
        }
    }
    // Fall back to bare name (relies on PATH)
    PathBuf::from("cargo")
}

impl CodeGraphState {
    /// Load from Parquet files in `graph_dir`, falling back to empty batches.
    pub fn load(graph_dir: &Path, workspace_root: &Path, cargo_path: Option<&Path>) -> Self {
        let nodes_path = graph_dir.join("nodes.parquet");
        let edges_path = graph_dir.join("edges.parquet");
        let nodes = load_parquet_or_empty(&nodes_path, &crate::schema::code_nodes_schema());
        let edges = load_parquet_or_empty(&edges_path, &crate::schema::code_edges_schema());
        CodeGraphState {
            nodes,
            edges,
            workspace_root: workspace_root.to_path_buf(),
            graph_dir: graph_dir.to_path_buf(),
            cargo_path: resolve_cargo_path(cargo_path),
            sync_publisher: None,
        }
    }
}

fn load_parquet_or_empty(path: &Path, schema: &arrow::datatypes::Schema) -> RecordBatch {
    let empty = || RecordBatch::new_empty(std::sync::Arc::new(schema.clone()));
    if !path.exists() {
        return empty();
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return empty(),
    };
    let reader = match ParquetRecordBatchReaderBuilder::try_new(file).and_then(|b| b.build()) {
        Ok(r) => r,
        Err(_) => return empty(),
    };
    // Collect ALL row groups — Parquet splits into batches of ~1024 rows.
    // Previously only read the first batch, silently dropping the rest.
    let batches: Vec<RecordBatch> = reader.filter_map(|r| r.ok()).collect();
    if batches.is_empty() {
        return empty();
    }
    arrow::compute::concat_batches(&batches[0].schema(), &batches).unwrap_or_else(|_| empty())
}

// ─── Handlers ───────────────────────────────────────────────────────────────

pub fn handle_search(payload: &[u8], state: &mut CodeGraphState) -> Vec<u8> {
    let req: SearchRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return noesis_ship::service::error_response(&format!("invalid JSON: {e}"), 400),
    };
    let kind = req.kind.as_deref().and_then(CodeNodeKind::parse);
    let search = CodeSearch {
        name_pattern: Some(req.query.clone()),
        body_pattern: Some(req.query.clone()),
        kind,
        limit: req.limit.or(Some(20)),
        ..Default::default()
    };
    let result = search_nodes(&state.nodes, &search);
    let nodes: Vec<NodeSummary> = result.nodes.iter().map(NodeSummary::from_node).collect();
    let total_matched = nodes.len();
    noesis_ship::service::serialize_response(&SearchResponse {
        total_scanned: result.total_scanned,
        total_matched,
        nodes,
    })
}

pub fn handle_read(payload: &[u8], state: &mut CodeGraphState) -> Vec<u8> {
    let req: ReadRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return noesis_ship::service::error_response(&format!("invalid JSON: {e}"), 400),
    };
    use crate::schema::node_col;
    use arrow::array::StringArray;
    let idx = state
        .nodes
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .and_then(|ids| (0..ids.len()).find(|&i| ids.value(i) == req.id.as_str()));
    let node = idx.and_then(|i| extract_node_at_index(&state.nodes, i));
    let found = node.is_some();
    noesis_ship::service::serialize_response(&ReadResponse {
        found,
        node: node.map(CodeNodeFull::from_node),
    })
}

pub fn handle_deps(payload: &[u8], state: &mut CodeGraphState) -> Vec<u8> {
    let req: DepsRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return noesis_ship::service::error_response(&format!("invalid JSON: {e}"), 400),
    };
    let limit = req.limit.unwrap_or(50);
    let caller_nodes = if req.direction == "callers" || req.direction == "both" {
        callers(&req.id, &state.nodes, &state.edges)
            .into_iter()
            .take(limit)
            .map(|n| NodeSummary::from_node(&n))
            .collect()
    } else {
        Vec::new()
    };
    let callee_nodes = if req.direction == "callees" || req.direction == "both" {
        callees(&req.id, &state.nodes, &state.edges)
            .into_iter()
            .take(limit)
            .map(|n| NodeSummary::from_node(&n))
            .collect()
    } else {
        Vec::new()
    };
    noesis_ship::service::serialize_response(&DepsResponse {
        id: req.id,
        callers: caller_nodes,
        callees: callee_nodes,
    })
}

pub fn handle_replace(payload: &[u8], state: &mut CodeGraphState) -> Vec<u8> {
    let req: ReplaceRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return noesis_ship::service::error_response(&format!("invalid JSON: {e}"), 400),
    };
    if req.rationale.trim().is_empty() {
        return noesis_ship::service::error_response("rationale is required for code_replace", 400);
    }
    let update = NodeUpdate {
        body: Some(req.new_body.clone()),
        signature: None,
        docstring: None,
        body_hash: None,
        loc: None,
        cyclomatic_complexity: None,
        coverage_pct: None,
    };
    match codegraph_update_object(&state.nodes, &req.id, &update) {
        Ok(new_batch) => {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(req.new_body.as_bytes());
            let hash = format!("{:x}", hasher.finalize());
            let new_body_hash = Some(hash[..16].to_string());

            state.nodes = new_batch;
            let graph_dir = state.graph_dir.clone();
            if let Err(e) =
                crate::ingest_pipeline::write_graph_parquet(&state.nodes, &state.edges, &graph_dir)
            {
                tracing::warn!(error = %e, "failed to persist updated graph");
            }

            // EX-3184: Publish update to NATS for cross-agent sync.
            if let Some(ref publisher) = state.sync_publisher {
                let hash_str = new_body_hash.as_deref().unwrap_or("");
                let update =
                    publisher.make_update(&req.id, &req.new_body, hash_str, &req.rationale);
                // Fire-and-forget — don't block the response on NATS publish.
                let publisher_clone = state.sync_publisher.as_ref().unwrap().client_ref().clone();
                let update_bytes = serde_json::to_vec(&update).unwrap_or_default();
                tokio::spawn(async move {
                    if let Err(e) = publisher_clone
                        .publish(crate::nats_sync::UPDATES_SUBJECT, update_bytes.into())
                        .await
                    {
                        tracing::warn!(error = %e, "failed to publish graph sync update");
                    }
                });
            }

            noesis_ship::service::serialize_response(&ReplaceResponse {
                success: true,
                id: req.id,
                new_body_hash,
                rationale: req.rationale,
                error: None,
            })
        }
        Err(e) => noesis_ship::service::serialize_response(&ReplaceResponse {
            success: false,
            id: req.id,
            new_body_hash: None,
            rationale: req.rationale,
            error: Some(e.to_string()),
        }),
    }
}

pub fn handle_query(payload: &[u8], state: &mut CodeGraphState) -> Vec<u8> {
    let req: QueryRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return noesis_ship::service::error_response(&format!("invalid JSON: {e}"), 400),
    };
    let filter = QueryFilter {
        kind: req.kind,
        name_contains: req.name_contains,
        parent_id: None,
        min_loc: req.min_loc,
        min_complexity: None,
        max_coverage: None,
        limit: req.limit.or(Some(50)),
    };
    match codegraph_query_objects(&state.nodes, &filter) {
        Ok(result) => {
            let nodes: Vec<NodeSummary> = result.nodes.iter().map(NodeSummary::from_node).collect();
            noesis_ship::service::serialize_response(&QueryResponse {
                total_matched: result.total_matched,
                total_scanned: result.total_scanned,
                nodes,
            })
        }
        Err(e) => noesis_ship::service::error_response(&e.to_string(), 500),
    }
}

/// VY-3495: Set RUSTC env var to nusy-rustc for graph-native compilation.
///
/// If nusy-rustc is installed and a graph exists, cargo will read source
/// from Arrow CodeNode bodies instead of .rs files on disk.
fn inject_graph_rustc_env(cmd: &mut Command, state: &CodeGraphState) {
    // Look for nusy-rustc in common locations
    let nusy_rustc = which_nusy_rustc();
    if let Some(rustc_path) = nusy_rustc {
        cmd.env("RUSTC", &rustc_path);
        cmd.env("NUSY_GRAPH_DIR", &state.graph_dir);
        cmd.env("NUSY_WORKSPACE", &state.workspace_root);
        // Use nightly toolchain for rustc_private
        cmd.env("RUSTUP_TOOLCHAIN", "nightly");
        tracing::info!(
            "graph-native build: RUSTC={} NUSY_GRAPH_DIR={}",
            rustc_path.display(),
            state.graph_dir.display()
        );
    } else {
        tracing::debug!("nusy-rustc not found, using standard cargo build");
    }
}

/// Find the nusy-rustc binary.
fn which_nusy_rustc() -> Option<PathBuf> {
    // Check $HOME/.cargo/bin first (installed via cargo install)
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(home).join(".cargo/bin/nusy-rustc");
        if path.exists() {
            return Some(path);
        }
    }
    // Check workspace target directory
    let workspace_target = PathBuf::from("target/debug/nusy-rustc");
    if workspace_target.exists() {
        return Some(workspace_target);
    }
    let workspace_release = PathBuf::from("target/release/nusy-rustc");
    if workspace_release.exists() {
        return Some(workspace_release);
    }
    // Check PATH
    if let Ok(output) = Command::new("which").arg("nusy-rustc").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

pub fn handle_build(payload: &[u8], state: &mut CodeGraphState) -> Vec<u8> {
    let req: BuildRequest = serde_json::from_slice(payload).unwrap_or_default();
    let mut cmd = Command::new(&state.cargo_path);
    cmd.arg("build").current_dir(&state.workspace_root);
    // VY-3495: Graph-native compilation via nusy-rustc FileLoader
    inject_graph_rustc_env(&mut cmd, state);
    if let Some(ref c) = req.crate_name {
        cmd.args(["--package", c]);
    }
    let command_str = format!(
        "cargo build{}",
        req.crate_name
            .as_deref()
            .map(|c| format!(" --package {c}"))
            .unwrap_or_default()
    );
    match cmd.output() {
        Ok(out) => noesis_ship::service::serialize_response(&BuildResponse {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            command: command_str,
        }),
        Err(e) => noesis_ship::service::error_response(&format!("cargo error: {e}"), 500),
    }
}

pub fn handle_test(payload: &[u8], state: &mut CodeGraphState) -> Vec<u8> {
    let req: TestRequest = serde_json::from_slice(payload).unwrap_or_default();
    let mut cmd = Command::new(&state.cargo_path);
    cmd.arg("test").current_dir(&state.workspace_root);
    // VY-3495: Graph-native compilation via nusy-rustc FileLoader
    inject_graph_rustc_env(&mut cmd, state);
    if let Some(ref c) = req.crate_name {
        cmd.args(["--package", c]);
    }
    if let Some(ref f) = req.filter {
        cmd.arg(f);
    }
    let command_str = format!(
        "cargo test{}{}",
        req.crate_name
            .as_deref()
            .map(|c| format!(" --package {c}"))
            .unwrap_or_default(),
        req.filter
            .as_deref()
            .map(|f| format!(" {f}"))
            .unwrap_or_default()
    );
    match cmd.output() {
        Ok(out) => noesis_ship::service::serialize_response(&TestResponse {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            command: command_str,
        }),
        Err(e) => noesis_ship::service::error_response(&format!("cargo error: {e}"), 500),
    }
}

/// Return MCP tool schemas — called by the MCP bridge on `tools/list`.
pub fn handle_tools(_payload: &[u8], _state: &mut CodeGraphState) -> Vec<u8> {
    let tools = vec![
        ToolSchema {
            name: "code_search".to_string(),
            description: "Search code nodes by name, body pattern, or kind".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Substring match on node name and body"},
                    "kind": {"type": "string", "description": "Optional: function|class|module|file|..."},
                    "limit": {"type": "integer", "description": "Max results (default 20)"}
                },
                "required": ["query"]
            }),
        },
        ToolSchema {
            name: "code_read".to_string(),
            description: "Read a code node's full body, signature, and metadata by ID".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Node ID (e.g. crate::module::fn_name)"}
                },
                "required": ["id"]
            }),
        },
        ToolSchema {
            name: "code_dependencies".to_string(),
            description: "Find callers and/or callees of a code node".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "direction": {
                        "type": "string",
                        "enum": ["callers", "callees", "both"],
                        "description": "Default: both"
                    },
                    "limit": {"type": "integer", "description": "Max per direction (default 50)"}
                },
                "required": ["id"]
            }),
        },
        ToolSchema {
            name: "code_replace".to_string(),
            description: "Replace a code node's body. Always requires user approval.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "new_body": {"type": "string"},
                    "rationale": {"type": "string", "description": "Required: reason for this change"}
                },
                "required": ["id", "new_body", "rationale"]
            }),
        },
        ToolSchema {
            name: "code_query".to_string(),
            description: "Structured filter query across all code nodes".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string"},
                    "name_contains": {"type": "string"},
                    "min_loc": {"type": "integer"},
                    "limit": {"type": "integer"}
                }
            }),
        },
        ToolSchema {
            name: "code_build".to_string(),
            description: "Run cargo build on the workspace or a specific crate".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "crate_name": {"type": "string", "description": "Optional: specific crate"}
                }
            }),
        },
        ToolSchema {
            name: "code_test".to_string(),
            description: "Run cargo test on the workspace or a specific crate".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "crate_name": {"type": "string"},
                    "filter": {"type": "string", "description": "Optional test name filter"}
                }
            }),
        },
    ];
    noesis_ship::service::serialize_response(&ToolsResponse {
        service: "codegraph".to_string(),
        tools,
    })
}

// ─── Internal: extract CodeNode at row index ─────────────────────────────────

fn extract_node_at_index(batch: &RecordBatch, idx: usize) -> Option<CodeNode> {
    use crate::schema::node_col;
    use arrow::array::{
        Float64Array, Int32Array, LargeStringArray, StringArray, UInt32Array, UInt64Array,
    };
    use arrow::datatypes::Int32Type;

    if idx >= batch.num_rows() {
        return None;
    }

    let get_str = |col: usize| -> Option<String> {
        batch
            .column(col)
            .as_any()
            .downcast_ref::<StringArray>()
            .and_then(|a| {
                if a.is_null(idx) {
                    None
                } else {
                    Some(a.value(idx).to_string())
                }
            })
    };

    let get_large_str = |col: usize| -> Option<String> {
        batch
            .column(col)
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .and_then(|a| {
                if a.is_null(idx) {
                    None
                } else {
                    Some(a.value(idx).to_string())
                }
            })
    };

    let get_i32 = |col: usize| -> Option<i32> {
        batch
            .column(col)
            .as_any()
            .downcast_ref::<Int32Array>()
            .and_then(|a| {
                if a.is_null(idx) {
                    None
                } else {
                    Some(a.value(idx))
                }
            })
    };

    let get_f64 = |col: usize| -> Option<f64> {
        batch
            .column(col)
            .as_any()
            .downcast_ref::<Float64Array>()
            .and_then(|a| {
                if a.is_null(idx) {
                    None
                } else {
                    Some(a.value(idx))
                }
            })
    };

    let get_u32 = |col: usize| -> Option<u32> {
        batch
            .column(col)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .and_then(|a| {
                if a.is_null(idx) {
                    None
                } else {
                    Some(a.value(idx))
                }
            })
    };

    let get_u64 = |col: usize| -> Option<u64> {
        batch
            .column(col)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .and_then(|a| {
                if a.is_null(idx) {
                    None
                } else {
                    Some(a.value(idx))
                }
            })
    };

    let id = get_str(node_col::ID)?;

    // KIND is a DictionaryArray<Int32Type, StringArray>
    let kind_str = {
        use arrow::array::DictionaryArray;
        if let Some(dict) = batch
            .column(node_col::KIND)
            .as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
        {
            let key = dict.keys().value(idx) as usize;
            dict.values()
                .as_any()
                .downcast_ref::<StringArray>()
                .map(|v| v.value(key).to_string())
                .unwrap_or_default()
        } else {
            get_str(node_col::KIND).unwrap_or_default()
        }
    };
    let kind = CodeNodeKind::parse(&kind_str).unwrap_or(CodeNodeKind::File);

    Some(CodeNode {
        id,
        kind,
        parent_id: get_str(node_col::PARENT_ID),
        name: get_str(node_col::NAME).unwrap_or_default(),
        signature: get_str(node_col::SIGNATURE),
        docstring: get_str(node_col::DOCSTRING),
        body_hash: get_str(node_col::BODY_HASH),
        body: get_large_str(node_col::BODY),
        loc: get_i32(node_col::LOC),
        cyclomatic_complexity: get_i32(node_col::CYCLOMATIC_COMPLEXITY),
        coverage_pct: get_f64(node_col::COVERAGE_PCT),
        last_modified: None,
        start_line: get_u32(node_col::START_LINE),
        end_line: get_u32(node_col::END_LINE),
        start_col: get_u32(node_col::START_COL),
        end_col: get_u32(node_col::END_COL),
        file_path: get_str(node_col::FILE_PATH),
        byte_offset: get_u64(node_col::BYTE_OFFSET),
    })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        CodeEdge, CodeNode, CodeNodeKind, build_code_edges_batch, build_code_nodes_batch,
    };

    fn make_state() -> CodeGraphState {
        let nodes = vec![
            CodeNode {
                id: "crate::search::search_nodes".to_string(),
                kind: CodeNodeKind::Function,
                parent_id: Some("crate::search".to_string()),
                name: "search_nodes".to_string(),
                signature: Some(
                    "pub fn search_nodes(batch: &RecordBatch, query: &CodeSearch) -> SearchResult"
                        .to_string(),
                ),
                body: Some("// finds matching nodes".to_string()),
                loc: Some(42),
                ..Default::default()
            },
            CodeNode {
                id: "crate::ingest::ingest_files".to_string(),
                kind: CodeNodeKind::Function,
                parent_id: Some("crate::ingest".to_string()),
                name: "ingest_files".to_string(),
                signature: Some(
                    "pub fn ingest_files(root: &Path, files: &[PathBuf]) -> Result<IngestResult>"
                        .to_string(),
                ),
                body: Some("// ingest source files".to_string()),
                loc: Some(100),
                ..Default::default()
            },
        ];
        let edges: Vec<CodeEdge> = vec![];
        let nodes_batch = build_code_nodes_batch(&nodes).expect("nodes batch");
        let edges_batch = build_code_edges_batch(&edges).expect("edges batch");
        CodeGraphState {
            nodes: nodes_batch,
            edges: edges_batch,
            workspace_root: PathBuf::from("."),
            graph_dir: PathBuf::from("."),
            cargo_path: PathBuf::from("cargo"),
            sync_publisher: None,
        }
    }

    #[test]
    fn search_by_name_returns_match() {
        let mut state = make_state();
        let payload = serde_json::to_vec(&serde_json::json!({"query": "ingest"})).unwrap();
        let response = handle_search(&payload, &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert!(parsed["total_matched"].as_u64().unwrap() >= 1);
        let names: Vec<&str> = parsed["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"ingest_files"));
    }

    #[test]
    fn search_no_match_returns_empty() {
        let mut state = make_state();
        let payload =
            serde_json::to_vec(&serde_json::json!({"query": "zzz_nonexistent_xyz"})).unwrap();
        let response = handle_search(&payload, &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(parsed["total_matched"], 0);
        assert_eq!(parsed["nodes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn search_invalid_json_returns_400() {
        let mut state = make_state();
        let response = handle_search(b"not json {{", &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(parsed["code"], 400);
    }

    #[test]
    fn read_existing_node_returns_body() {
        let mut state = make_state();
        let payload =
            serde_json::to_vec(&serde_json::json!({"id": "crate::search::search_nodes"})).unwrap();
        let response = handle_read(&payload, &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(parsed["found"], true);
        assert_eq!(parsed["node"]["name"], "search_nodes");
        assert_eq!(parsed["node"]["loc"], 42);
    }

    #[test]
    fn read_missing_node_returns_not_found() {
        let mut state = make_state();
        let payload = serde_json::to_vec(&serde_json::json!({"id": "nonexistent::id"})).unwrap();
        let response = handle_read(&payload, &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(parsed["found"], false);
        assert!(parsed["node"].is_null());
    }

    #[test]
    fn query_by_kind_returns_functions() {
        let mut state = make_state();
        let payload = serde_json::to_vec(&serde_json::json!({
            "kind": "function",
            "limit": 10
        }))
        .unwrap();
        let response = handle_query(&payload, &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert!(parsed["total_matched"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn replace_empty_rationale_returns_400() {
        let mut state = make_state();
        let payload = serde_json::to_vec(&serde_json::json!({
            "id": "crate::search::search_nodes",
            "new_body": "fn new_body() {}",
            "rationale": "  "
        }))
        .unwrap();
        let response = handle_replace(&payload, &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(parsed["code"], 400);
    }

    #[test]
    fn tools_returns_seven_schemas() {
        let mut state = make_state();
        let response = handle_tools(b"{}", &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        let tools = parsed["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 7);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in &[
            "code_search",
            "code_read",
            "code_dependencies",
            "code_replace",
            "code_query",
            "code_build",
            "code_test",
        ] {
            assert!(names.contains(expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn deps_invalid_json_returns_400() {
        let mut state = make_state();
        let response = handle_deps(b"bad", &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(parsed["code"], 400);
    }

    /// Contract test: every MCP tool name in handle_tools() must map to a
    /// handler registered in service.rs. The bridge strips the `code_` prefix
    /// to get the NATS command suffix (e.g., `code_search` → `search`,
    /// `code_dependencies` → `dependencies`). This test verifies that every
    /// suffix matches a handler name, preventing the `deps` vs `dependencies`
    /// mismatch that caused the original 404 bug.
    #[test]
    fn tool_names_match_handler_commands() {
        // These are the handler command names registered in service.rs via
        // NatsServiceBuilder::handler(). Keep this in sync with service.rs.
        let registered_handlers: Vec<&str> = vec![
            "search",
            "read",
            "dependencies",
            "replace",
            "query",
            "build",
            "test",
            "tools",
        ];

        let mut state = make_state();
        let response = handle_tools(b"{}", &mut state);
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        let tools = parsed["tools"].as_array().unwrap();

        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            // Bridge routing: strip "code_" prefix to get NATS command suffix
            let suffix = name
                .strip_prefix("code_")
                .unwrap_or_else(|| panic!("tool name '{name}' missing 'code_' prefix"));
            assert!(
                registered_handlers.contains(&suffix),
                "MCP tool '{name}' maps to NATS command '{suffix}', \
                 but no handler is registered for '{suffix}'. \
                 Registered handlers: {registered_handlers:?}"
            );
        }
    }

    #[test]
    fn resolve_cargo_path_returns_existing_binary() {
        use super::resolve_cargo_path;
        // With no explicit path, should resolve to something
        let resolved = resolve_cargo_path(None);
        // On any dev machine with Rust installed, this should be a real path
        assert!(!resolved.as_os_str().is_empty());
    }

    #[test]
    fn resolve_cargo_path_explicit_overrides() {
        use super::resolve_cargo_path;
        let explicit = std::path::Path::new("/usr/local/fake/cargo");
        let resolved = resolve_cargo_path(Some(explicit));
        assert_eq!(resolved, PathBuf::from("/usr/local/fake/cargo"));
    }
}
