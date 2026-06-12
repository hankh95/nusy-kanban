//! Echo service example — demonstrates NatsServiceBuilder.
//!
//! Run with: `cargo run --example echo_service -- --nats-url nats://localhost:4222`
//!
//! Test with: `nats req echo.cmd.echo '{"message": "hello"}'`
//!            `nats req echo.cmd.count ''`

use clap::Parser;
use noesis_ship::service::{NatsServiceBuilder, ServiceArgs};

#[derive(Default)]
struct EchoState {
    request_count: u64,
}

#[tokio::main]
async fn main() -> noesis_ship::types::Result<()> {
    tracing_subscriber::fmt::init();

    let args = ServiceArgs::parse();

    NatsServiceBuilder::new("echo.cmd", EchoState::default())
        .nats_url(&args.nats_url)
        .handler("echo", |payload, _state| payload.to_vec())
        .handler("count", |_payload, state: &mut EchoState| {
            state.request_count += 1;
            noesis_ship::service::serialize_response(&state.request_count)
        })
        .handler("hello", |_payload, _state| {
            noesis_ship::service::serialize_response(&serde_json::json!({
                "message": "Hello from noesis-ship!"
            }))
        })
        .on_shutdown(|state| {
            println!("Shutting down after {} requests", state.request_count);
        })
        .run()
        .await
}
