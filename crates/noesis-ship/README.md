# noesis-ship

[![Crates.io](https://img.shields.io/crates/v/noesis-ship.svg)](https://crates.io/crates/noesis-ship)
[![Documentation](https://docs.rs/noesis-ship/badge.svg)](https://docs.rs/noesis-ship)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)

Rust NATS communication platform for multi-agent AI systems.

> **Name disambiguation.** This crate (`noesis-ship` on crates.io) is the **Rust**
> NATS layer, published from the private NuSy monorepo (public home: [nusy.dev](https://nusy.dev)).
> It is distinct from the original [**`noesis-ship` Python/Node platform**](https://github.com/hankh95/noesis-ship)
> (the canonical multi-agent comms platform, consumed by NuSy via `git+https`),
> which is not on PyPI. Same name, two implementations — this is the Rust one.

## Features

**Seven building blocks** over [NATS](https://nats.io):

| Primitive | Transport | Use Case |
|-----------|-----------|----------|
| **PubSub** | NATS Core | Fire-and-forget broadcast (heartbeats, status) |
| **EventBus** | JetStream | Durable event streaming with 24h replay |
| **Channels** | JetStream | Point-to-point messaging with history |
| **KV Store** | NATS KV | Shared state with watch, TTL, history |
| **Object Store** | NATS Object Store | Large blob storage with SHA-256 |
| **JobQueue** | In-memory (NATS KV planned) | Generic job lifecycle (queued → running → complete/failed) |
| **NatsServiceBuilder** | NATS Core | Build a request-reply service in ~20 lines |

Add to your `Cargo.toml`:

```toml
[dependencies]
noesis-ship = "0.14"
```

## Quick Start — ConnectionManager

All primitives start with a connection:

```rust
use noesis_ship::connection::ConnectionManager;
use noesis_ship::types::NatsConfig;

let config = NatsConfig::new("nats://localhost:4222");
let mut conn = ConnectionManager::new(config);
conn.connect().await?;

let client = conn.client()?;        // NATS Core client
let js = conn.jetstream()?;         // JetStream context
conn.ensure_stream(&stream).await?; // Create stream if missing
```

## PubSub — Fire-and-Forget

Raw NATS Core publish/subscribe. No persistence — if nobody is listening,
the message is lost.

```rust
use noesis_ship::pubsub::{Publisher, Subscriber, EventType};
use noesis_ship::types::NatsConfig;

// Publisher
let mut pub_ = Publisher::new("agent-1", NatsConfig::default());
pub_.connect().await?;
pub_.emit(&EventType::Heartbeat, json!({"status": "ok"})).await?;
pub_.emit(&EventType::Custom("deploy.started".into()), json!({"version": "1.2"})).await?;

// Subscriber
let mut sub = Subscriber::new("monitor", NatsConfig::default());
sub.connect().await?;
sub.subscribe("ship.events.>", |event| async move {
    println!("{}: {}", event.event_type, event.payload);
});
sub.run().await?;
```

## EventBus — Durable Events (JetStream)

Events are persisted to a JetStream stream. Late-joining consumers replay
recent history. Default: `SHIP_EVENTS` stream, 24h retention, 100k max.

```rust
use noesis_ship::event_bus::EventBus;
use noesis_ship::types::NatsConfig;

let bus = EventBus::new(NatsConfig::default())
    .with_source("kanban-server");
bus.connect().await?;

// Emit — persisted to JetStream
bus.emit_event("item.created", json!({"id": "EX-3001"})).await?;

// Subscribe with durable consumer — replays missed events
bus.subscribe("item.*", "dashboard-consumer", |event| {
    Box::pin(async move {
        println!("[{}] {} from {}", event.timestamp, event.event_type, event.source);
    })
}).await?;
```

### Custom stream

```rust
use noesis_ship::types::StreamConfig;

let stream = StreamConfig::new("MY_EVENTS", vec!["myapp.events.>".into()])
    .with_max_age(3600)     // 1 hour retention
    .with_max_msgs(10_000)
    .with_memory_storage();

let bus = EventBus::with_stream(NatsConfig::default(), stream, "myapp.events");
```

## Channels — Point-to-Point Messaging

JetStream-backed channels with history replay and own-message filtering.

```rust
use noesis_ship::channels::ChannelService;
use noesis_ship::types::NatsConfig;

let mut ch = ChannelService::new(NatsConfig::default());
ch.connect("agent-1").await?;

// Send
ch.send_message("dev", "deployment complete", None).await?;
ch.send_message("dev", "tests green", Some(json!({"ci": true}))).await?;

// Subscribe (replay_history = true to get past messages)
ch.subscribe("dev", true, |msg| {
    Box::pin(async move {
        println!("[{}] {}: {}", msg.channel, msg.sender, msg.content);
    })
}).await?;

// Fetch history
let history = ch.get_channel_history("dev", 50).await?;
```

## KV Store — Shared State

NATS KV buckets with watch, TTL, and history. Three built-in specializations:

```rust
use noesis_ship::kv::KvStore;
use noesis_ship::types::{NatsConfig, KvBucketConfig};

// Generic KV
let config = KvBucketConfig::new("my_bucket")
    .with_history(5)
    .with_ttl_secs(3600);
let kv = KvStore::new(config, NatsConfig::default());
kv.connect().await?;

kv.put("key1", &json!({"count": 42})).await?;
let val = kv.get("key1").await?;   // Some({"count": 42})
let keys = kv.keys().await?;       // ["key1"]
kv.delete("key1").await?;

// Watch for changes (real-time)
let mut stream = kv.watch().await?;
while let Some((key, value)) = stream.next().await {
    println!("{key} changed: {value}");
}
```

### Built-in specializations

```rust
use noesis_ship::kv::{BeingRegistry, ShipConfig, HealthMetrics, BeingState};

// Being registry — track agent status
let registry = BeingRegistry::new(NatsConfig::default());
registry.connect().await?;
registry.register("agent-1", vec!["rust".into(), "training".into()]).await?;
registry.update_status("agent-1", BeingState::Working, Some("EX-3001".into())).await?;
registry.heartbeat("agent-1").await?;
let online = registry.get_online().await?;

// Ship config — shared configuration
let config = ShipConfig::new(NatsConfig::default());
config.connect().await?;
config.set("log_level", &json!("debug")).await?;
let level = config.get("log_level", json!("info")).await?;

// Health metrics — TTL-based health reporting
let health = HealthMetrics::new(NatsConfig::default());
health.connect().await?;
health.report("agent-1", &json!({"cpu": 0.45, "mem_mb": 1200})).await?;
```

## Object Store — Large Blobs

NATS Object Store for files and snapshots with SHA-256 integrity.

```rust
use noesis_ship::object_store::ShipObjectStore;
use noesis_ship::types::NatsConfig;

let store = ShipObjectStore::new("artifacts", NatsConfig::default());
store.connect().await?;

// Store bytes
let meta = store.put("model.safetensors", &bytes, Some("LoRA adapter")).await?;
println!("SHA-256: {}", meta.sha256);

// Store file
let meta = store.put_file("data.parquet", Path::new("/tmp/data.parquet"), None).await?;

// Retrieve
let data = store.get("model.safetensors").await?; // Option<Vec<u8>>

// List
let objects = store.list().await?;
```

### Built-in specializations

```rust
use noesis_ship::object_store::{BeingSnapshots, ArtifactStore};

// Being snapshots
let snaps = BeingSnapshots::new(NatsConfig::default());
snaps.connect().await?;
let id = snaps.take("agent-1", &json!({"state": "..."}), "checkpoint").await?;
let state = snaps.restore(&id).await?;

// Artifact store (code, logs, docs)
let arts = ArtifactStore::new(NatsConfig::default());
arts.connect().await?;
arts.store_artifact("report.html", &html_bytes, "report", Some("EX-3001")).await?;
```

## JobQueue — Generic Job Lifecycle

Track work items through `queued → running → complete | failed`. Workers claim
jobs atomically (filtered by worker name). Any serde-able payload type works.

```rust
use noesis_ship::job_queue::{JobQueue, JobStatus};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildJob {
    repo: String,
    branch: String,
}

let mut queue = JobQueue::<BuildJob>::new("BUILD");

// Submit a job targeted at a specific worker
let id = queue.submit(
    BuildJob { repo: "myapp".into(), branch: "main".into() },
    "ci-server",   // target worker
    "developer-1", // queued by
);

// Worker claims next available job
let job = queue.claim("ci-server").unwrap();
let job_id = job.id.clone();

// Complete with result (or fail with error)
queue.complete(&job_id, serde_json::json!({"artifact": "build/out.tar"}));
// queue.fail(&job_id, "compilation error");

// List and filter
let queued = queue.list(Some(&JobStatus::Queued));
let (q, r, c, f) = queue.counts();
```

**Real-world usage:** NuSy's training queue uses `JobQueue<TrainingPayload>` to
coordinate GPU training runs across a fleet of machines — any agent queues jobs,
DGX claims and executes them.

## NatsServiceBuilder — Request-Reply Services

Build a NATS service with routing, mutation events, and graceful shutdown:

```rust
use noesis_ship::service::NatsServiceBuilder;

NatsServiceBuilder::new("myservice.cmd", MyState::default())
    .nats_url("nats://localhost:4222")
    .handler("echo", |payload, _state| payload.to_vec())
    .handler("count", |_payload, state: &mut MyState| {
        state.count += 1;
        serde_json::to_vec(&state.count).unwrap_or_default()
    })
    .on_shutdown(|state| state.save())
    .run()
    .await?;
```

### Catch-all dispatch + JetStream events

For services with many commands, use `default_handler()` to delegate to an
existing dispatch function. Add `event_bus_stream()` for durable event publishing:

```rust
use noesis_ship::service::NatsServiceBuilder;
use noesis_ship::types::StreamConfig;

let events = StreamConfig::new("MY_EVENTS", vec!["myservice.event.>".into()]);

NatsServiceBuilder::new("myservice.cmd", state)
    .nats_url("nats://localhost:4222")
    .default_handler(|subject, payload, state| dispatch(subject, payload, state))
    .mutation_callback(|cmd, resp, _| detect_mutation(cmd, resp))
    .event_prefix("myservice.event")
    .event_bus_stream(events, "myservice")
    .on_shutdown(persist_state)
    .run()
    .await?;
```

Named handlers take precedence over the default handler. Events published via
`event_bus_stream` use JetStream with `ShipEvent` envelopes; without it, events
use fire-and-forget PubSub.

### ServiceArgs

Standard CLI arguments for services:

```rust
use noesis_ship::service::ServiceArgs;
use clap::Parser;

let args = ServiceArgs::parse();
// args.data_dir  — PathBuf, default "."
// args.nats_url  — String, default "nats://localhost:4222"
```

## Error Handling

All operations return `noesis_ship::types::Result<T>`:

```rust
use noesis_ship::types::Error;

match result {
    Err(Error::NotConnected) => reconnect(),
    Err(Error::Timeout(d)) => retry_after(d),
    Err(Error::KeyNotFound(k)) => create_default(k),
    Err(Error::JetStream(msg)) => log_js_error(msg),
    Err(e) => bail!("unexpected: {e}"),
    Ok(v) => use_value(v),
}
```

## Requirements

- Rust 1.85+ (edition 2024)
- NATS server 2.10+ (with JetStream enabled for EventBus, Channels, KV, Object Store)

## License

MIT — Copyright (c) Hank Head / Congruent Systems PBC
