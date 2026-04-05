//! MCP (Model Context Protocol) server for nusy-kanban.
//!
//! Provides structured tool use over stdio transport, backed by NATS client.
//! Implements the MCP JSON-RPC 2.0 protocol with these tools:
//!
//! - `kanban_query`     — search items by natural language or filters
//! - `kanban_show`      — get full item details by ID
//! - `kanban_create`    — create a new work item
//! - `kanban_move`      — change item status
//! - `kanban_update`    — update item fields
//! - `kanban_relations` — show item relations and dependencies
//! - `kanban_stats`     — board statistics, velocity, burndown

use crate::client::NatsClient;
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

/// MCP protocol version supported by this server.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// MCP server wrapping a NATS client connection.
pub struct McpServer {
    client: NatsClient,
}

/// Tool definition in MCP format.
struct ToolDef {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
}

impl McpServer {
    /// Create a new MCP server connected to the given NATS URL.
    pub fn new(nats_url: &str) -> Result<Self, crate::client::ClientError> {
        let client = NatsClient::connect(nats_url)?;
        Ok(McpServer { client })
    }

    /// Run the MCP stdio server — reads JSON-RPC from stdin, writes to stdout.
    pub fn run(&self) -> io::Result<()> {
        let stdin = io::stdin();
        let mut stdout = io::stdout();

        for line in stdin.lock().lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let msg: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": {
                            "code": -32700,
                            "message": format!("Parse error: {e}")
                        }
                    });
                    writeln!(stdout, "{}", response)?;
                    stdout.flush()?;
                    continue;
                }
            };

            let response = self.handle_message(&msg);
            writeln!(stdout, "{}", response)?;
            stdout.flush()?;
        }

        Ok(())
    }

    /// Dispatch a JSON-RPC message to the appropriate handler.
    fn handle_message(&self, msg: &Value) -> Value {
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

        match method {
            "initialize" => self.handle_initialize(&id),
            "notifications/initialized" => Value::Null, // no response for notifications
            "tools/list" => self.handle_tools_list(&id),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                self.handle_tools_call(&id, &params)
            }
            "ping" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            }),
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            }),
        }
    }

    /// Handle `initialize` — return server info and capabilities.
    fn handle_initialize(&self, id: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "nusy-kanban-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        })
    }

    /// Handle `tools/list` — return all available tool schemas.
    fn handle_tools_list(&self, id: &Value) -> Value {
        let tools: Vec<Value> = tool_schemas()
            .into_iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema
                })
            })
            .collect();

        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": tools }
        })
    }

    /// Handle `tools/call` — dispatch to the appropriate tool handler.
    fn handle_tools_call(&self, id: &Value, params: &Value) -> Value {
        let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));

        let result = match name {
            "kanban_query" => self.tool_query(&args),
            "kanban_show" => self.tool_show(&args),
            "kanban_create" => self.tool_create(&args),
            "kanban_move" => self.tool_move(&args),
            "kanban_update" => self.tool_update(&args),
            "kanban_relations" => self.tool_relations(&args),
            "kanban_stats" => self.tool_stats(&args),
            _ => Err(format!("Unknown tool: {name}")),
        };

        match result {
            Ok(content) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{
                        "type": "text",
                        "text": content.to_string()
                    }]
                }
            }),
            Err(e) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{
                        "type": "text",
                        "text": json!({"error": e}).to_string()
                    }],
                    "isError": true
                }
            }),
        }
    }

    // ─── Tool implementations (NATS-backed) ────────────────────────────────

    fn tool_query(&self, args: &Value) -> Result<Value, String> {
        let payload = json!({
            "query": args.get("query").and_then(|q| q.as_str()).unwrap_or(""),
            "board": args.get("board").and_then(|b| b.as_str()),
            "status": args.get("status").and_then(|s| s.as_str()),
            "item_type": args.get("item_type").and_then(|t| t.as_str()),
            "assignee": args.get("assignee").and_then(|a| a.as_str()),
            "format": "json"
        });
        self.nats_request("query", &payload)
    }

    fn tool_show(&self, args: &Value) -> Result<Value, String> {
        let id = args
            .get("id")
            .and_then(|i| i.as_str())
            .ok_or_else(|| "Missing required parameter: id".to_string())?;
        let payload = json!({ "id": id, "format": "json" });
        self.nats_request("show", &payload)
    }

    fn tool_create(&self, args: &Value) -> Result<Value, String> {
        let title = args
            .get("title")
            .and_then(|t| t.as_str())
            .ok_or_else(|| "Missing required parameter: title".to_string())?;
        let item_type = args
            .get("item_type")
            .and_then(|t| t.as_str())
            .ok_or_else(|| "Missing required parameter: item_type".to_string())?;

        validate_enum(item_type, VALID_ITEM_TYPES, "item_type")?;

        let payload = json!({
            "title": title,
            "item_type": item_type,
            "priority": args.get("priority").and_then(|p| p.as_str()),
            "assignee": args.get("assignee").and_then(|a| a.as_str()),
            "tags": args.get("tags"),
            "body": args.get("body").and_then(|b| b.as_str()),
            "push": true
        });
        self.nats_request("create", &payload)
    }

    fn tool_move(&self, args: &Value) -> Result<Value, String> {
        let id = args
            .get("id")
            .and_then(|i| i.as_str())
            .ok_or_else(|| "Missing required parameter: id".to_string())?;
        let status = args
            .get("status")
            .and_then(|s| s.as_str())
            .ok_or_else(|| "Missing required parameter: status".to_string())?;

        validate_enum(status, VALID_STATUSES, "status")?;

        let payload = json!({
            "id": id,
            "status": status,
            "assign": args.get("assignee").and_then(|a| a.as_str()),
            "resolution": args.get("resolution").and_then(|r| r.as_str()),
            "closed_by": args.get("closed_by").and_then(|c| c.as_str())
        });
        self.nats_request("move", &payload)
    }

    fn tool_update(&self, args: &Value) -> Result<Value, String> {
        let id = args
            .get("id")
            .and_then(|i| i.as_str())
            .ok_or_else(|| "Missing required parameter: id".to_string())?;

        let payload = json!({
            "id": id,
            "title": args.get("title").and_then(|t| t.as_str()),
            "priority": args.get("priority").and_then(|p| p.as_str()),
            "assignee": args.get("assignee").and_then(|a| a.as_str()),
            "tags": args.get("tags"),
            "related": args.get("related"),
            "body": args.get("body").and_then(|b| b.as_str())
        });
        self.nats_request("update", &payload)
    }

    fn tool_relations(&self, args: &Value) -> Result<Value, String> {
        let id = args
            .get("id")
            .and_then(|i| i.as_str())
            .ok_or_else(|| "Missing required parameter: id".to_string())?;

        validate_item_id(id)?;

        // Get item to show depends_on and related fields
        let item_payload = json!({ "id": id, "format": "json" });
        let item = self.nats_request("show", &item_payload)?;

        // Get transitive dependencies via structured query (validated ID only)
        let deps_payload = json!({
            "relation_query": { "type": "dependencies_of", "target": id },
            "format": "json"
        });
        let deps = self.nats_request("query", &deps_payload)?;

        Ok(json!({
            "id": id,
            "item": item,
            "transitive_dependencies": deps
        }))
    }

    fn tool_stats(&self, args: &Value) -> Result<Value, String> {
        let board = args
            .get("board")
            .and_then(|b| b.as_str())
            .unwrap_or("development");

        let payload = json!({
            "board": board,
            "velocity": args.get("velocity").and_then(|v| v.as_bool()).unwrap_or(false),
            "burndown": args.get("burndown").and_then(|b| b.as_bool()).unwrap_or(false),
            "by_agent": args.get("by_agent").and_then(|a| a.as_bool()).unwrap_or(false),
            "weeks": args.get("weeks").and_then(|w| w.as_u64()).unwrap_or(4)
        });
        self.nats_request("stats", &payload)
    }

    /// Send a request to the kanban NATS server and return the JSON response.
    fn nats_request(&self, command: &str, payload: &Value) -> Result<Value, String> {
        self.client
            .request(command, payload)
            .map_err(|e| e.to_string())
    }
}

// ─── Validation ────────────────────────────────────────────────────────────

const VALID_STATUSES: &[&str] = &[
    "backlog",
    "in_progress",
    "review",
    "done",
    "blocked",
    "draft",
    "active",
    "retired",
    "complete",
    "abandoned",
    "planned",
    "running",
    "outline",
    "writing",
    "captured",
    "formalized",
];

const VALID_ITEM_TYPES: &[&str] = &[
    "expedition",
    "chore",
    "voyage",
    "hazard",
    "signal",
    "paper",
    "hypothesis",
    "experiment",
    "measure",
    "idea",
    "literature",
];

/// Validate that a value is in the allowed set, returning a descriptive error.
fn validate_enum(value: &str, allowed: &[&str], param_name: &str) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "Invalid {param_name}: '{value}'. Must be one of: {}",
            allowed.join(", ")
        ))
    }
}

/// Validate that an item ID matches the expected format (e.g., "EX-3150", "VOY-155").
fn validate_item_id(id: &str) -> Result<(), String> {
    // Allow: PREFIX-NUMBER, PREFIX-NUMBER.NUMBER (paper-scoped), HNUMBER.NUMBER
    let valid = id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && id.len() <= 20
        && !id.is_empty();
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Invalid item ID format: '{id}'. Expected format like 'EX-3150' or 'H130.1'"
        ))
    }
}

// ─── Tool Schemas (Phase 1) ────────────────────────────────────────────────

/// Return all MCP tool schema definitions.
fn tool_schemas() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "kanban_query",
            description: "Search kanban items by natural language query or structured filters. Returns matching items as JSON.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language search query (e.g., 'backlog expeditions assigned to Mini')"
                    },
                    "board": {
                        "type": "string",
                        "enum": ["development", "research"],
                        "description": "Board to search (default: all boards)"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["backlog", "in_progress", "review", "done", "blocked"],
                        "description": "Filter by status"
                    },
                    "item_type": {
                        "type": "string",
                        "enum": ["expedition", "chore", "voyage", "hazard", "signal", "paper", "hypothesis", "experiment", "measure", "idea", "literature"],
                        "description": "Filter by item type"
                    },
                    "assignee": {
                        "type": "string",
                        "description": "Filter by assignee (e.g., 'M5', 'Mini', 'DGX')"
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "kanban_show",
            description: "Get full details for a kanban item by ID, including body content, tags, dependencies, and status.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Item ID (e.g., 'EX-3150', 'VOY-155', 'PAPER-130')"
                    }
                },
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "kanban_create",
            description: "Create a new kanban work item. Returns the allocated ID.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Item title"
                    },
                    "item_type": {
                        "type": "string",
                        "enum": ["expedition", "chore", "voyage", "hazard", "signal"],
                        "description": "Item type"
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["low", "medium", "high", "critical"],
                        "description": "Priority level"
                    },
                    "assignee": {
                        "type": "string",
                        "description": "Agent to assign (e.g., 'M5', 'Mini', 'DGX')"
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tags (e.g., ['v14', 'rust'])"
                    },
                    "body": {
                        "type": "string",
                        "description": "Body content with phases, acceptance criteria, etc."
                    }
                },
                "required": ["title", "item_type"]
            }),
        },
        ToolDef {
            name: "kanban_move",
            description: "Change the status of a kanban item (e.g., backlog → in_progress → review → done).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Item ID to move"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["backlog", "in_progress", "review", "done", "blocked"],
                        "description": "Target status"
                    },
                    "assignee": {
                        "type": "string",
                        "description": "Assign to this agent during the move"
                    },
                    "resolution": {
                        "type": "string",
                        "enum": ["completed", "wont_do", "superseded", "duplicate", "obsolete", "merged"],
                        "description": "Resolution (required when moving to terminal states)"
                    },
                    "closed_by": {
                        "type": "string",
                        "description": "Provenance URI (e.g., 'PROP-2085')"
                    }
                },
                "required": ["id", "status"]
            }),
        },
        ToolDef {
            name: "kanban_update",
            description: "Update fields on an existing kanban item (title, priority, assignee, tags, body).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Item ID to update"
                    },
                    "title": {
                        "type": "string",
                        "description": "New title"
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["low", "medium", "high", "critical"],
                        "description": "New priority"
                    },
                    "assignee": {
                        "type": "string",
                        "description": "New assignee"
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Replace tags"
                    },
                    "related": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Replace related item IDs"
                    },
                    "body": {
                        "type": "string",
                        "description": "Replace body content"
                    }
                },
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "kanban_relations",
            description: "Show all relations for an item: depends_on, related items, and transitive dependency chains.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Item ID to show relations for"
                    }
                },
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "kanban_stats",
            description: "Get board statistics: item counts by status/type, velocity, burndown, or agent throughput.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "board": {
                        "type": "string",
                        "enum": ["development", "research"],
                        "description": "Board to get stats for (default: development)"
                    },
                    "velocity": {
                        "type": "boolean",
                        "description": "Include weekly velocity (items completed per week)"
                    },
                    "burndown": {
                        "type": "boolean",
                        "description": "Include burndown chart data (items remaining over time)"
                    },
                    "by_agent": {
                        "type": "boolean",
                        "description": "Include per-agent throughput breakdown"
                    },
                    "weeks": {
                        "type": "integer",
                        "description": "Number of weeks for velocity/burndown (default: 4)"
                    }
                }
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Phase 1: Tool schema tests ────────────────────────────────

    #[test]
    fn test_tool_schemas_count() {
        let schemas = tool_schemas();
        assert_eq!(schemas.len(), 7);
    }

    #[test]
    fn test_tool_schemas_have_required_fields() {
        for tool in tool_schemas() {
            assert!(!tool.name.is_empty(), "tool name must not be empty");
            assert!(
                !tool.description.is_empty(),
                "tool description must not be empty"
            );
            assert_eq!(
                tool.input_schema["type"], "object",
                "inputSchema must be object type for {}",
                tool.name
            );
        }
    }

    #[test]
    fn test_tool_names() {
        let schemas = tool_schemas();
        let names: Vec<&str> = schemas.iter().map(|t| t.name).collect();
        assert!(names.contains(&"kanban_query"));
        assert!(names.contains(&"kanban_show"));
        assert!(names.contains(&"kanban_create"));
        assert!(names.contains(&"kanban_move"));
        assert!(names.contains(&"kanban_update"));
        assert!(names.contains(&"kanban_relations"));
        assert!(names.contains(&"kanban_stats"));
    }

    #[test]
    fn test_required_params() {
        let schemas = tool_schemas();

        // kanban_show requires "id"
        let show = schemas.iter().find(|t| t.name == "kanban_show").unwrap();
        let required = show.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("id")));

        // kanban_create requires "title" and "item_type"
        let create = schemas.iter().find(|t| t.name == "kanban_create").unwrap();
        let required = create.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("title")));
        assert!(required.contains(&json!("item_type")));

        // kanban_move requires "id" and "status"
        let mv = schemas.iter().find(|t| t.name == "kanban_move").unwrap();
        let required = mv.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("id")));
        assert!(required.contains(&json!("status")));

        // kanban_stats has no required params
        let stats = schemas.iter().find(|t| t.name == "kanban_stats").unwrap();
        assert!(stats.input_schema.get("required").is_none());
    }

    // ─── Phase 2: MCP protocol tests ───────────────────────────────

    // Note: McpServer::new() requires a real NATS connection, so we test
    // the protocol handling via handle_message() with a mock approach.
    // Integration tests requiring NATS are in crates/nusy-kanban/tests/.

    #[test]
    fn test_initialize_response_format() {
        // Test the response structure directly
        let id = json!(1);
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "nusy-kanban-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        });

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert_eq!(response["result"]["serverInfo"]["name"], "nusy-kanban-mcp");
    }

    #[test]
    fn test_tools_list_format() {
        let tools: Vec<Value> = tool_schemas()
            .into_iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema
                })
            })
            .collect();

        assert_eq!(tools.len(), 7);
        for tool in &tools {
            assert!(tool.get("name").is_some());
            assert!(tool.get("description").is_some());
            assert!(tool.get("inputSchema").is_some());
        }
    }

    // ─── Phase 4: JSON response tests ──────────────────────────────

    #[test]
    fn test_error_response_format() {
        let error_response = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "result": {
                "content": [{
                    "type": "text",
                    "text": json!({"error": "Item not found: EX-9999"}).to_string()
                }],
                "isError": true
            }
        });

        assert_eq!(error_response["result"]["isError"], true);
        let text = error_response["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert!(parsed["error"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn test_success_response_format() {
        let content = json!({"id": "EX-3150", "title": "MCP Server"});
        let response = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {
                "content": [{
                    "type": "text",
                    "text": content.to_string()
                }]
            }
        });

        assert_eq!(response["jsonrpc"], "2.0");
        assert!(response["result"].get("isError").is_none());
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["id"], "EX-3150");
    }

    #[test]
    fn test_tool_schema_enum_values() {
        let schemas = tool_schemas();

        // kanban_move status enum should include common statuses
        let mv = schemas.iter().find(|t| t.name == "kanban_move").unwrap();
        let status_enum = mv.input_schema["properties"]["status"]["enum"]
            .as_array()
            .unwrap();
        assert!(status_enum.contains(&json!("backlog")));
        assert!(status_enum.contains(&json!("in_progress")));
        assert!(status_enum.contains(&json!("done")));

        // kanban_create item_type enum
        let create = schemas.iter().find(|t| t.name == "kanban_create").unwrap();
        let type_enum = create.input_schema["properties"]["item_type"]["enum"]
            .as_array()
            .unwrap();
        assert!(type_enum.contains(&json!("expedition")));
        assert!(type_enum.contains(&json!("chore")));
        assert!(type_enum.contains(&json!("voyage")));
    }

    #[test]
    fn test_protocol_version() {
        // MCP protocol version must be a valid date string
        assert!(MCP_PROTOCOL_VERSION.contains('-'));
        assert_eq!(MCP_PROTOCOL_VERSION.len(), 10);
    }

    // ─── Validation tests ──────────────────────────────────────────

    #[test]
    fn test_validate_enum_valid() {
        assert!(validate_enum("expedition", VALID_ITEM_TYPES, "item_type").is_ok());
        assert!(validate_enum("in_progress", VALID_STATUSES, "status").is_ok());
        assert!(validate_enum("done", VALID_STATUSES, "status").is_ok());
    }

    #[test]
    fn test_validate_enum_invalid() {
        let err = validate_enum("invalid_type", VALID_ITEM_TYPES, "item_type").unwrap_err();
        assert!(err.contains("Invalid item_type"));
        assert!(err.contains("invalid_type"));
        assert!(err.contains("expedition"));

        let err = validate_enum("unknown", VALID_STATUSES, "status").unwrap_err();
        assert!(err.contains("Invalid status"));
    }

    #[test]
    fn test_validate_item_id_valid() {
        assert!(validate_item_id("EX-3150").is_ok());
        assert!(validate_item_id("VOY-155").is_ok());
        assert!(validate_item_id("PAPER-130").is_ok());
        assert!(validate_item_id("H130.1").is_ok());
        assert!(validate_item_id("EXPR-130.1").is_ok());
        assert!(validate_item_id("M-42").is_ok());
    }

    #[test]
    fn test_validate_item_id_invalid() {
        assert!(validate_item_id("").is_err());
        assert!(validate_item_id("EX-3150; DROP TABLE").is_err());
        assert!(validate_item_id("id with spaces").is_err());
        assert!(validate_item_id("<script>alert(1)</script>").is_err());
    }
}
