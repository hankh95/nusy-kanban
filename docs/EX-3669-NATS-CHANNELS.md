# NATS Channel Conventions — Multi-Being Coordination

> **Expedition:** EX-3669
> **Scope:** Documents all NATS subjects used for multi-being coordination across the NuSy fleet (Mini, M5, DGX).
> **Canonical source:** `crates/nusy-kanban/src/client.rs`, `crates/nusy-kanban-server/src/events.rs`, `crates/nusy-kanban-server/src/handlers/mod.rs`, `crates/nusy-kanban/src/nats_training_queue.rs`, `crates/nusy-training/src/gate_event_emitter.rs`, `scripts/fleet-train.sh`

---

## 1. Subject Naming Convention

All subjects follow a `{namespace}.{entity}.{action}` structure:

| Namespace | Layer | Used by |
|-----------|-------|---------|
| `kanban.cmd.*` | Request-reply | All `nk` CLI commands |
| `kanban.event.*` | Pub-sub (JetStream) | Kanban mutations |
| `training.gate.*` | Pub-sub (raw NATS) | Curriculum gate review events |
| `ship.training.*` | Pub-sub (raw NATS) | Legacy fleet training scripts |
| `training_queue` | NATS KV (bucket) | Distributed GPU job queue |

**No subject hierarchy is shared between namespaces.** `kanban.event.>` and `ship.training.*` are independent trees.

---

## 2. Command / Response Subjects (Request-Reply)

**Pattern:** Client sends a JSON payload to `kanban.cmd.{command}` and receives a JSON response.

**Implementation:** `crates/nusy-kanban/src/client.rs` — `NatsClient::request()` sends on `kanban.cmd.{command}` with a 30-second timeout.

### Core Commands

| Subject | Request payload | Response |
|---------|-----------------|----------|
| `kanban.cmd.create` | `{"item_type":"expedition","title":"...","board":"development","body":"..."}` | `{"id":"EXP-3001",...}` |
| `kanban.cmd.move` | `{"id":"EXP-3001","to":"in_progress","resolution":"","closed_by":""}` | `{"id":"EXP-3001","from":"backlog","to":"in_progress"}` |
| `kanban.cmd.update` | `{"id":"EXP-3001","title":"...","priority":"high",...}` | `{"id":"EXP-3001",...}` |
| `kanban.cmd.comment` | `{"id":"EXP-3001","body":"..."}` | `{"id":"EXP-3001","comment_id":"...",...}` |
| `kanban.cmd.list` | `{"board":"development","status":"in_progress",...}` | `{"items":[...]}` |
| `kanban.cmd.show` | `{"id":"EXP-3001"}` | Full item JSON |
| `kanban.cmd.board` | `{}` | Board column view |
| `kanban.cmd.query` | `{"query":"backlog expeditions","top":10}` | Search results |
| `kanban.cmd.stats` | `{"filter":"in_progress"}` | Statistics |
| `kanban.cmd.delete` | `{"id":"EXP-3001"}` | `{"id":"EXP-3001"}` |
| `kanban.cmd.validate` | `{}` | Validation report |
| `kanban.cmd.export` | `{"format":"json"}` | Exported data |
| `kanban.cmd.roadmap` | `{"flat":false}` | Voyage-grouped view |
| `kanban.cmd.critical-path` | `{}` | Dependency chains |
| `kanban.cmd.worklist` | `{"agents":["Mini","M5","DGX"],"depth":3}` | Per-agent assignments |
| `kanban.cmd.next-id` | `{"item_type":"expedition"}` | `{"next_id":"EXP-3002"}` |
| `kanban.cmd.history` | `{"weeks":4}` | Recently completed items |
| `kanban.cmd.blocked` | `{}` | Blocked items |
| `kanban.cmd.templates` | `{}` | Available templates |

### Research Board (HDD) Commands

| Subject | Creates |
|---------|---------|
| `kanban.cmd.hdd.paper` | `PAPER-*` items |
| `kanban.cmd.hdd.hypothesis` | `H-*` items |
| `kanban.cmd.hdd.experiment` | `EXPR-*` items |
| `kanban.cmd.hdd.measure` | `M-*` items |
| `kanban.cmd.hdd.idea` | `IDEA-*` items |
| `kanban.cmd.hdd.literature` | `LIT-*` items |
| `kanban.cmd.hdd.validate` | — (validation report) |
| `kanban.cmd.hdd.registry` | — (item registry) |
| `kanban.cmd.hdd.run` | Experiment run record |
| `kanban.cmd.hdd.run.status` | Run status |
| `kanban.cmd.hdd.run.complete` | Run completion |

### Relations Commands

| Subject | Purpose |
|---------|---------|
| `kanban.cmd.relation.add` | Add item relationship |
| `kanban.cmd.relation.query` | Query relationships |

### Proposal (PR) Commands

| Subject | Purpose |
|---------|---------|
| `kanban.cmd.pr.create` | Create proposal |
| `kanban.cmd.pr.list` | List open proposals |
| `kanban.cmd.pr.view` | View proposal detail |
| `kanban.cmd.pr.diff` | Graph-native diff |
| `kanban.cmd.pr.review` | Approve or request changes |
| `kanban.cmd.pr.merge` | Merge proposal |
| `kanban.cmd.pr.close` | Close without merge |
| `kanban.cmd.pr.comment` | Add review comment |
| `kanban.cmd.pr.checks` | Safety gate status |
| `kanban.cmd.pr.revise` | Re-enter review after rejection |
| `kanban.cmd.pr.resolve` | Resolve a comment thread |
| `kanban.cmd.pr.ci_store` | Store CI results |

### Source / Git Commands (planned for VY-3009 Phase 2)

| Subject | Status |
|---------|--------|
| `kanban.cmd.source.push` | Acknowledged, not implemented |
| `kanban.cmd.source.pull` | Acknowledged, not implemented |
| `kanban.cmd.source.branches` | Acknowledged, not implemented |
| `kanban.cmd.source.delete` | Acknowledged, not implemented |
| `kanban.cmd.git.*` | Placeholder — operates on local graph store |

### Error Responses

Errors return a JSON envelope with `error` and `code` fields:

```json
{
  "error": "item not found",
  "code": "NOT_FOUND"
}
```

Possible error codes: `NOT_FOUND`, `INVALID_PAYLOAD`, `UNKNOWN_COMMAND`, `INTERNAL`.

---

## 3. Event Broadcast Subjects (Pub-Sub via JetStream)

**Pattern:** Server publishes to `kanban.event.*` after every mutation. Events are wrapped in a `ShipEvent` envelope and persisted to the `KANBAN_EVENTS` JetStream stream.

**Stream config:** `KANBAN_EVENTS`, subjects `kanban.event.>`, 24h retention, 100k max messages, file storage.

**Wildcard subscription:** Subscribe to `kanban.event.>` to receive all mutation events.

### Implemented Events

| Subject | Triggered by | Payload fields |
|---------|--------------|----------------|
| `kanban.event.created` | `create`, `hdd.*` commands | `id`, `title`, `item_type`, `board`, `agent` |
| `kanban.event.moved` | `move` command | `id`, `from`, `to`, `agent` |
| `kanban.event.deleted` | `delete` command | `id` |
| `kanban.event.stats` | Periodic broadcast | `total_items`, `active_items`, `by_status`, `timestamp` |

### ShipEvent Envelope

All events published via JetStream use the `ShipEvent` envelope from `noesis-ship`:

```json
{
  "event_type": "kanban.item.created",
  "timestamp": "2026-04-05T14:30:00.000Z",
  "source": "kanban-server",
  "payload": {
    "id": "EXP-3001",
    "title": "Tools Registry",
    "item_type": "expedition",
    "board": "development",
    "agent": null
  },
  "correlation_id": "a1b2c3d4",
  "version": "1.0"
}
```

**Mutation detection:** `crates/nusy-kanban-server/src/events.rs` — `detect_mutation()` maps command names to event types. Only non-error responses from `create`/`move`/`delete` (and their HDD variants) emit events.

---

## 4. Training Queue Subjects (NATS KV)

The training queue uses **NATS KV** (key-value bucket), not pub-sub subjects.

**Bucket name:** `training_queue`
**Key pattern:** `job.{TRAIN-XXX}` (e.g., `job.TRAIN-001`)

**Operations:**

```bash
# Queue a job (CLI)
nk training queue EXPR-3275 --being santiago-bahai --corpus bahai --machine DGX

# Claim next queued job (DGX)
nk training claim --machine DGX

# Mark complete
nk training complete TRAIN-001 --results research/shared/eval-data/expr3275/

# Mark failed
nk training fail TRAIN-001 --error "OOM at epoch 3"
```

**KV job record structure:**

```json
{
  "id": "TRAIN-001",
  "payload": {
    "experiment_id": "EXPR-3275",
    "being": "santiago-bahai",
    "corpus": "bahai"
  },
  "worker": "DGX",
  "queued_by": "Mini",
  "status": "queued",
  "queued_at": "1743868800",
  "started_at": null,
  "completed_at": null,
  "error": null,
  "result": null
}
```

**Status values:** `queued`, `running`, `complete`, `failed`

---

## 5. Training Event Subjects (Pub-Sub — Raw NATS)

These are published by shell scripts and Rust training code directly, not through the kanban server.

| Subject | Emitted by | Payload |
|---------|-----------|---------|
| `ship.training.started` | `scripts/fleet-train.sh` | JSON: `session_id`, `being`, `agent`, `args`, `timestamp` |
| `ship.training.complete` | `scripts/fleet-train.sh` | JSON: full metrics including `exit_code`, `duration_seconds`, `documents`, `triples` |
| `ship.training.failed` | `scripts/fleet-train.sh` | JSON: same as complete, plus `error` |
| `training.gate.review_needed` | `crates/nusy-training/src/gate_event_emitter.rs` | JSON: `being_id`, `phase_id`, `domain`, `bloom_level`, `attempt_history`, `threshold` |

### Example: `ship.training.complete` payload

```json
{
  "session_id": "train-20260405-143000-12345",
  "being": "santiago-bahai",
  "agent": "Mini",
  "exit_code": 0,
  "duration_seconds": 3600,
  "documents": 1523,
  "triples": 48291,
  "started_at": "2026-04-05T14:00:00Z",
  "finished_at": "2026-04-05T15:00:00Z",
  "args": "--curriculum L2_grade_school"
}
```

### Example: `training.gate.review_needed` payload

```json
{
  "being_id": "santiago-bahai-v14.2",
  "phase_id": 3,
  "domain": "science",
  "bloom_level": "Understand",
  "attempt_history": [0.700000, 0.720000],
  "threshold": 0.750000
}
```

---

## 6. Code Examples

### Subscribe to `kanban.event.>` in Rust (via noesis-ship)

```rust
// crates/nusy-kanban-server/src/events.rs — already implemented
use noesis_ship::event_bus::EventBus;

async fn subscribe_kanban_events(nats_url: &str) -> noesis_ship::types::Result<()> {
    let mut bus = EventBus::new(nats_url).await?;
    bus.subscribe("kanban.event.>", |event: ShipEvent| {
        println!("Received event: {:?}", event.event_type);
        Box::pin(async {})
    }).await?;
    bus.run().await;
    Ok(())
}
```

### Subscribe to `kanban.event.>` in Python (via noesis-ship)

```python
# Python consumer via noesis-ship's EventBus bindings
import asyncio
from noesis_ship.core import ShipEventBus, ShipEvent

async def on_kanban_event(event: ShipEvent):
    print(f"Event: {event.event_type} from {event.source}")
    payload = event.payload
    if event.event_type == "kanban.item.created":
        print(f"  New item: {payload.get('id')} — {payload.get('title')}")
    elif event.event_type == "kanban.item.moved":
        print(f"  Moved: {payload.get('id')} from {payload.get('from')} to {payload.get('to')}")

async def main():
    bus = ShipEventBus(nats_url="nats://192.168.8.110:4222")
    await bus.subscribe("kanban.event.>", on_kanban_event)
    print("Listening on kanban.event.> ...")
    await asyncio.Event().wait()  # keep running

asyncio.run(main())
```

### Publish a command in Rust

```rust
// crates/nusy-kanban/src/client.rs — already implemented
use async_nats::Client;

async fn create_item(client: &Client) -> Result<serde_json::Value, ClientError> {
    let subject = "kanban.cmd.create";
    let payload = serde_json::json!({
        "item_type": "expedition",
        "title": "Tools Registry",
        "board": "development"
    });

    let response = client.request(subject, payload.into()).await?;
    Ok(response)
}
```

### Publish a command in Python (via noesis-ship)

```python
# Python client via noesis-ship NATS bindings
import asyncio
from noesis_ship.core import NATSClient

async def main():
    client = NATSClient("nats://192.168.8.110:4222")
    await client.connect()

    payload = {
        "item_type": "expedition",
        "title": "Tools Registry",
        "board": "development"
    }

    response = await client.request("kanban.cmd.create", payload)
    print(f"Created: {response['id']}")

asyncio.run(main())
```

### Subscribe to training events in Rust

```rust
// crates/nusy-training/src/gate_event_emitter.rs — gate review events
use noesis_ship::event_bus::EventBus;

async fn subscribe_training_gates(nats_url: &str) -> noesis_ship::types::Result<()> {
    let mut bus = EventBus::new(nats_url).await?;
    bus.subscribe("training.gate.review_needed", |event: ShipEvent| {
        let payload = &event.payload;
        eprintln!(
            "Gate review needed: being={} phase={} domain={}",
            payload.get("being_id").unwrap(),
            payload.get("phase_id").unwrap(),
            payload.get("domain").unwrap(),
        );
        Box::pin(async {})
    }).await?;
    bus.run().await;
    Ok(())
}
```

---

## 7. Gaps and Formalization Recommendations

### Gap 1: `agent` Field Not Populated in Events

**Current:** `ItemCreated.agent`, `ItemMoved.agent` are `None` in all events (hardcoded `None` in `detect_mutation()`).

**Should:** Pass the requesting agent's identity through the request-reply chain and populate `agent` in emitted events. This requires:
1. Adding `agent` to the request payload (e.g., `--assign "Mini"` already sets assignee, but the agent identity is not in the command payload)
2. Threading it through `ServerState` and into `detect_mutation()`

**Proposed request field:** Add `"agent": "Mini"` to all command payloads so events can record who triggered the mutation.

### Gap 2: `stats` Event Is Defined But Never Emitted

**Current:** `StatsSnapshot` and `kanban.event.stats` are defined in `events.rs` but `detect_mutation()` has no path for stats — it is never published.

**Should:** Either implement a periodic publisher (every N minutes via a background task in the server) or remove the dead code. Periodic stats events would allow Command Deck to refresh its dashboard without polling.

### Gap 3: Training Events Use Raw NATS, Not JetStream

**Current:** `ship.training.*` events are published via `nats pub` shell command — no durability, no replay for late-joining consumers.

**Should:** Either:
- Move training events through the `KANBAN_EVENTS` JetStream stream (add `training.event.>` subjects)
- Or create a separate `TRAINING_EVENTS` JetStream stream for `ship.training.>` with `deliver_policy=all` for replay

### Gap 4: No Formal Subject Constants Enum

**Current:** Subject strings are defined as `&str` constants scattered across `events.rs` (`subjects::CREATED`, etc.) and as string literals in handlers.

**Should:** Centralize all subject constants in one `nusy_kanban::nats::subjects` module:

```rust
// crates/nusy-kanban/src/nats/subjects.rs
pub mod cmd {
    pub const CREATE: &str = "kanban.cmd.create";
    pub const MOVE: &str = "kanban.cmd.move";
    pub const LIST: &str = "kanban.cmd.list";
    pub const SHOW: &str = "kanban.cmd.show";
    // ... all others
}
pub mod event {
    pub const CREATED: &str = "kanban.event.created";
    pub const MOVED: &str = "kanban.event.moved";
    pub const DELETED: &str = "kanban.event.deleted";
    pub const STATS: &str = "kanban.event.stats";
    pub const WILDCARD: &str = "kanban.event.>";
}
```

### Gap 5: No Schema Validation on Event Payloads

**Current:** Events use `#[derive(Serialize)]` structs and `serde_json::Value` interchangeably. No JSON Schema exists.

**Should:** Add a `kanban.event.schema.*` subject tree or a `SCHEMA.md` document with canonical JSON examples for every event type (this document fills that gap for now).

### Gap 6: `hdd.run` Commands Are Ad-Hoc

**Current:** `hdd.run`, `hdd.run.status`, `hdd.run.complete` are implemented in `handlers/research.rs` but have no corresponding event emission.

**Should:** Add `hdd.run.started`, `hdd.run.completed` events to the event tree for experiment run tracking visibility.

### Summary of All Subjects

| Subject | Type | Durability | Description |
|---------|------|------------|-------------|
| `kanban.cmd.*` | Request-reply | None | All kanban CLI commands |
| `kanban.event.created` | Pub-sub (JetStream) | KANBAN_EVENTS stream | Item created |
| `kanban.event.moved` | Pub-sub (JetStream) | KANBAN_EVENTS stream | Item moved |
| `kanban.event.deleted` | Pub-sub (JetStream) | KANBAN_EVENTS stream | Item deleted |
| `kanban.event.stats` | Pub-sub (JetStream) | KANBAN_EVENTS stream | Periodic stats (defined, not emitted) |
| `training_queue` (KV bucket) | NATS KV | KV bucket | Distributed GPU job queue |
| `ship.training.started` | Pub-sub (raw) | None | Training session started |
| `ship.training.complete` | Pub-sub (raw) | None | Training session succeeded |
| `ship.training.failed` | Pub-sub (raw) | None | Training session failed |
| `training.gate.review_needed` | Pub-sub (raw) | None | Curriculum gate exhausted retries |
