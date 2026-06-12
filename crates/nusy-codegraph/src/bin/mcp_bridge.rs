//! nusy-mcp-bridge — Generic NATS-to-MCP bridge.
//!
//! Exposes NATS services as Model Context Protocol (MCP) tools to Claude Code
//! and other MCP clients. Uses JSON-RPC 2.0 over stdio.
//!
//! # Usage
//!
//! ```bash
//! nusy-mcp-bridge --nats nats://192.168.8.110:4222 --services codegraph
//! ```
//!
//! # Claude Code settings.json
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "nusy-code": {
//!       "command": "nusy-mcp-bridge",
//!       "args": ["--nats", "nats://192.168.8.110:4222", "--services", "codegraph"]
//!     }
//!   }
//! }
//! ```

use clap::Parser;
use nusy_codegraph::mcp_bridge::McpBridge;

#[derive(Parser)]
#[command(name = "nusy-mcp-bridge", about = "Generic MCP-to-NATS bridge")]
struct Args {
    /// NATS server URL.
    #[arg(long, default_value = "nats://localhost:4222")]
    nats: String,

    /// Comma-separated list of services to expose.
    /// Supported: codegraph
    #[arg(long, default_value = "codegraph")]
    services: String,

    /// Request timeout in milliseconds.
    #[arg(long, default_value = "5000")]
    timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    // MCP bridges communicate over stdio — log to stderr so stdout stays clean
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nusy_codegraph=info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();

    let mut bridge = McpBridge::new(&args.nats).timeout_ms(args.timeout_ms);

    for service in args.services.split(',') {
        let service = service.trim();
        match service {
            "codegraph" => {
                bridge = bridge.service("codegraph.cmd", "code_");
            }
            other => {
                eprintln!("warning: unknown service '{other}', skipping");
            }
        }
    }

    bridge.run_stdio().await
}
