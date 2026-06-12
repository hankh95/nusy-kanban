//! Generic MCP-to-NATS bridge.
//!
//! `McpBridge` exposes any NATS service as Model Context Protocol (MCP) tools.
//! It runs as a stdio MCP server (JSON-RPC 2.0 over stdin/stdout), routing
//! `tools/list` and `tools/call` requests to the correct NATS services.
//!
//! # Protocol
//!
//! MCP uses JSON-RPC 2.0 over stdio. Each message is a newline-delimited JSON object.
//!
//! **tools/list flow:**
//! 1. Client sends `{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}`
//! 2. Bridge sends `{service}.cmd.tools` to each configured NATS service
//! 3. Returns combined tool list from all services
//!
//! **tools/call flow:**
//! 1. Client sends `{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"code_search","arguments":{...}}}`
//! 2. Bridge routes to `{service}.cmd.{tool_suffix}` via NATS request/reply
//! 3. Returns NATS response as MCP content
//!
//! # Tool naming convention
//!
//! NATS handler commands use underscore-separated suffixes (e.g. `codegraph.cmd.search`).
//! MCP tool names use the full `code_{command}` format (e.g. `code_search`).
//! The bridge maps: `code_search` → `codegraph.cmd.search`
//!
//! # Binary
//!
//! ```bash
//! nusy-mcp-bridge --nats nats://192.168.8.110:4222 --services codegraph
//! ```

use serde_json::Value;
use std::io::{self, BufRead, Write};
use std::time::Duration;

/// Configuration for a single NATS service exposed via MCP.
#[derive(Clone)]
pub struct ServiceConfig {
    /// NATS subject prefix (e.g. "codegraph.cmd").
    pub subject_prefix: String,
    /// MCP tool name prefix (e.g. "code_").
    pub tool_prefix: String,
}

impl ServiceConfig {
    pub fn new(subject_prefix: &str, tool_prefix: &str) -> Self {
        ServiceConfig {
            subject_prefix: subject_prefix.to_string(),
            tool_prefix: tool_prefix.to_string(),
        }
    }
}

/// Generic MCP-to-NATS bridge.
///
/// Discovers tool schemas from NATS services and routes tool calls.
pub struct McpBridge {
    nats_url: String,
    services: Vec<ServiceConfig>,
    /// Request timeout in milliseconds.
    timeout_ms: u64,
}

impl McpBridge {
    pub fn new(nats_url: &str) -> Self {
        McpBridge {
            nats_url: nats_url.to_string(),
            services: Vec::new(),
            timeout_ms: 5000,
        }
    }

    /// Add a NATS service to expose as MCP tools.
    pub fn service(mut self, subject_prefix: &str, tool_prefix: &str) -> Self {
        self.services
            .push(ServiceConfig::new(subject_prefix, tool_prefix));
        self
    }

    /// Set request timeout in milliseconds.
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    /// Run the bridge over stdio until EOF.
    ///
    /// Reads JSON-RPC requests from stdin, writes responses to stdout.
    pub async fn run_stdio(self) -> Result<(), String> {
        let client = async_nats::connect(&self.nats_url)
            .await
            .map_err(|e| format!("NATS connection failed: {e}"))?;

        tracing::info!(url = %self.nats_url, services = self.services.len(), "MCP bridge ready");

        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut out = io::BufWriter::new(stdout.lock());

        for line in stdin.lock().lines() {
            let line = line.map_err(|e| format!("stdin read: {e}"))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let request: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    let err_resp = json_error(Value::Null, -32700, &format!("parse error: {e}"));
                    writeln!(
                        out,
                        "{}",
                        serde_json::to_string(&err_resp).unwrap_or_default()
                    )
                    .ok();
                    out.flush().ok();
                    continue;
                }
            };

            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let params = request.get("params").cloned().unwrap_or(Value::Null);

            let response = match method {
                "initialize" => handle_initialize(id.clone()),
                "tools/list" => self.handle_tools_list(&client, id.clone()).await,
                "tools/call" => self.handle_tools_call(&client, id.clone(), &params).await,
                other => json_error(id.clone(), -32601, &format!("method not found: {other}")),
            };

            writeln!(
                out,
                "{}",
                serde_json::to_string(&response).unwrap_or_default()
            )
            .map_err(|e| format!("stdout write: {e}"))?;
            out.flush().map_err(|e| format!("stdout flush: {e}"))?;
        }

        Ok(())
    }

    /// Fetch tool schemas from all configured NATS services.
    async fn handle_tools_list(&self, client: &async_nats::Client, id: Value) -> Value {
        let mut all_tools: Vec<Value> = Vec::new();

        for svc in &self.services {
            let tools_subject = format!("{}.tools", svc.subject_prefix);
            let timeout = Duration::from_millis(self.timeout_ms);

            match tokio::time::timeout(timeout, client.request(tools_subject, "{}".into())).await {
                Ok(Ok(msg)) => {
                    // Parse the ToolsResponse from noesis_service
                    if let Ok(resp) = serde_json::from_slice::<Value>(&msg.payload)
                        && let Some(tools) = resp.get("tools").and_then(|t| t.as_array())
                    {
                        for tool in tools {
                            // Map tool name to MCP format: "code_search" → prefixed name
                            let mcp_tool = serde_json::json!({
                                "name": tool["name"],
                                "description": tool["description"],
                                "inputSchema": tool["input_schema"]
                            });
                            all_tools.push(mcp_tool);
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(subject = %svc.subject_prefix, error = %e, "tools fetch failed");
                }
                Err(_) => {
                    tracing::warn!(subject = %svc.subject_prefix, "tools fetch timed out");
                }
            }
        }

        json_result(id, serde_json::json!({ "tools": all_tools }))
    }

    /// Route a tool call to the correct NATS service.
    async fn handle_tools_call(
        &self,
        client: &async_nats::Client,
        id: Value,
        params: &Value,
    ) -> Value {
        let tool_name = match params.get("name").and_then(|n| n.as_str()) {
            Some(n) => n,
            None => return json_error(id, -32602, "missing 'name' in params"),
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        // Route: find which service owns this tool name
        let route = self.route_tool(tool_name);
        let (subject, _svc) = match route {
            Some(r) => r,
            None => {
                return json_error(id, -32602, &format!("unknown tool: {tool_name}"));
            }
        };

        let payload = serde_json::to_vec(&arguments).unwrap_or_else(|_| b"{}".to_vec());
        let timeout = Duration::from_millis(self.timeout_ms);

        match tokio::time::timeout(timeout, client.request(subject.clone(), payload.into())).await {
            Ok(Ok(msg)) => {
                let content_text = String::from_utf8_lossy(&msg.payload).to_string();
                json_result(
                    id,
                    serde_json::json!({
                        "content": [{"type": "text", "text": content_text}],
                        "isError": false
                    }),
                )
            }
            Ok(Err(e)) => json_error(id, -32603, &format!("NATS error: {e}")),
            Err(_) => json_error(id, -32603, &format!("timeout calling {subject}")),
        }
    }

    /// Find the NATS subject for a given MCP tool name.
    ///
    /// Maps `code_search` → `(codegraph.cmd.search, &ServiceConfig)`.
    fn route_tool(&self, tool_name: &str) -> Option<(String, &ServiceConfig)> {
        for svc in &self.services {
            if tool_name.starts_with(&svc.tool_prefix) {
                let suffix = tool_name
                    .strip_prefix(&svc.tool_prefix)
                    .unwrap_or(tool_name);
                let subject = format!("{}.{}", svc.subject_prefix, suffix);
                return Some((subject, svc));
            }
        }
        None
    }
}

// ─── MCP protocol helpers ────────────────────────────────────────────────────

fn handle_initialize(id: Value) -> Value {
    json_result(
        id,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "nusy-mcp-bridge",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn json_result(id: Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn json_error(id: Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_codegraph_tool() {
        let bridge = McpBridge::new("nats://localhost:4222").service("codegraph.cmd", "code_");

        let (subject, _) = bridge.route_tool("code_search").unwrap();
        assert_eq!(subject, "codegraph.cmd.search");

        let (subject, _) = bridge.route_tool("code_dependencies").unwrap();
        assert_eq!(subject, "codegraph.cmd.dependencies");

        let (subject, _) = bridge.route_tool("code_build").unwrap();
        assert_eq!(subject, "codegraph.cmd.build");
    }

    #[test]
    fn route_unknown_tool_returns_none() {
        let bridge = McpBridge::new("nats://localhost:4222").service("codegraph.cmd", "code_");
        assert!(bridge.route_tool("unknown_tool").is_none());
    }

    #[test]
    fn route_multiple_services() {
        let bridge = McpBridge::new("nats://localhost:4222")
            .service("codegraph.cmd", "code_")
            .service("kanban.cmd", "kb_");

        let (subj, _) = bridge.route_tool("code_read").unwrap();
        assert_eq!(subj, "codegraph.cmd.read");

        let (subj, _) = bridge.route_tool("kb_create").unwrap();
        assert_eq!(subj, "kanban.cmd.create");
    }

    #[test]
    fn handle_initialize_returns_correct_version() {
        let resp = handle_initialize(serde_json::json!(1));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn json_result_shape() {
        let r = json_result(serde_json::json!(42), serde_json::json!({"key": "val"}));
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], 42);
        assert_eq!(r["result"]["key"], "val");
    }

    #[test]
    fn json_error_shape() {
        let e = json_error(serde_json::json!(1), -32601, "not found");
        assert_eq!(e["error"]["code"], -32601);
        assert_eq!(e["error"]["message"], "not found");
    }
}
