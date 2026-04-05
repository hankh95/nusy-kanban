# nusy-kanban

[![Crates.io](https://img.shields.io/crates/v/nusy-kanban)](https://crates.io/crates/nusy-kanban)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Arrow-native kanban with a nautical soul** — a high-performance, multi-agent work
tracker built on Apache Arrow and Parquet. Tracks expeditions, voyages, and research
with graph-native PRs, dual boards (dev + research), and NATS-powered coordination.

For AI developers building autonomous agents, `nusy-kanban` provides a structured
research workflow (HDD), SPARQL-queryable metadata, and a training queue — everything
a being needs to own its own development pipeline.

## For AI Developers

This crate is designed for two audiences:

1. **Teams** — developers who want a fast, self-hosted kanban with NATS multi-agent
   coordination, crash-safe persistence, and zero-copy columnar queries.
2. **Autonomous AI beings** — agents who need a structured development workflow with
   hypothesis tracking, experiment queuing, and measurable targets.

If you are building a being that manages its own work, start with the
[Hypothesis-Driven Development (HDD) guide](#hdd-for-autonomous-beings) below.

---

## Quick Start

```bash
cargo install nusy-kanban

# Initialize (creates .nusy-kanban/ locally)
nusy-kanban init

# Create your first expedition
alias nk='nusy-kanban'   # or --server nats://your-host:4222 for multi-agent
nk create expedition "My First Feature" \
  --body "Phase 1: Design. Phase 2: Implement. Phase 3: Test." \
  --push

# View and move work
nk board
nk move EX-3001 in_progress --assign "dev"
```

### Multi-Agent Setup

Point all agents at a shared NATS server for single-writer semantics:

```bash
alias nk='nusy-kanban --server nats://192.168.8.110:4222'
nk create expedition "Team Feature" --push
```

No lock files, no ID collisions, no merge conflicts. The server serializes all writes.

---

## Core Concepts

### Arrow-Native Storage

Every write goes to an Apache Arrow RecordBatch backed by Parquet snapshots.
Queries are zero-copy columnar scans — no YAML globbing, no file I/O on reads.
Crash safety comes from a WAL + atomic rename pattern (via `nusy-arrow-git`).

### Dual Boards

| Board | Types | Purpose |
|-------|-------|---------|
| **Development** | Expedition, Chore, Voyage, Hazard, Signal | Feature work |
| **Research** | Paper, Hypothesis, Experiment, Measure, Idea, Literature | HDD research cycle |

### The Nautical Theme

Development items follow a nautical lifecycle:

```
Harbor → Provisioning → Underway → Approaching Port → Arrived
(backlog)   (ready)     (in_progress)  (review)        (done)
```

### Arrow Schema

```
┌─────────────────────────────────────────────────────────────────┐
│  items  (KanbanStore)                                            │
│  ─────────────────────────────────────────────────────────────── │
│  id, title, item_type, status, priority, assignee,               │
│  tags, related, depends_on, body, created_at, updated_at,         │
│  body_hash, resolution, closed_by                                │
└──────────────────────────┬────────────────────────────────────────┘
                           │ 1:many via id
        ┌──────────────────┼──────────────────┐
        ▼                  ▼                  ▼
┌───────────────┐  ┌───────────────┐  ┌───────────────┐
│  runs         │  │ item_comments  │  │  relations    │
│ ─────────────  │  │ ─────────────  │  │ ─────────────  │
│ id, item_id, │  │ id, item_id,  │  │ source_id,    │
│ status,       │  │ comment,      │  │ target_id,    │
│ updated_at,   │  │ author,        │  │ predicate,    │
│ updated_by,   │  │ created_at     │  │ source_type,  │
│ run_number    │  │                │  │ target_type   │
└───────────────┘  └───────────────┘  └───────────────┘
```

### Persistence Flow

```
Write Request
     │
     ▼
 nusy-arrow-git::save_named_batches()
     │
     ├─▶ Write to WAL (append-only log)
     │
     └─▶ Write to tmp Parquet files
             │
             ▼
         atomic rename()  ← crash-safe
             │
             ▼
     Final Parquet files in .nusy-kanban/
```

---

## HDD for Autonomous Beings

HDD (Hypothesis-Driven Development) is a research methodology that applies
test-driven development rigor to scientific investigation. Where TDD writes a
failing test first, HDD writes a falsifiable hypothesis before running an experiment.

The key rule: **only validated enhancements ship.** Negative results are documented,
not hidden.

### The 6 Research Types

All research items live on the **research board** and are created with `nk` commands:

| Type | ID | Purpose | Auto-links |
|------|----|---------|------------|
| **Paper** | `PAPER-{N}` | Publication documenting validated hypotheses | Root of chain |
| **Hypothesis** | `H{paper}.{seq}` | Falsifiable claim with quantitative target | `kb:tests` → Paper |
| **Experiment** | `EXPR-{paper}.{seq}` | Reproducible protocol | `kb:validates` → Hypothesis |
| **Measure** | `M-{N}` | Quantitative metric | `kb:measures` → Experiment |
| **Idea** | `IDEA-{N}` | Raw observation or question | None |
| **Literature** | `LIT-{N}` | Prior work survey | None |

### The Cycle

```
IDEA → LITERATURE → HYPOTHESIS → EXPERIMENT → ANALYSIS → PAPER
                                                  ↓
                                          FAIL? → Refine → loop
```

### Example: A Being Tracks Entity Recall

A being named Santiago notices entity queries are slow and runs an experiment:

```bash
# 1. Capture the observation
nk create idea "v14.2 entity recall is poor — fastembed might outperform graph traversal" \
    --tags "perception,v14.2" --board research --push
# → IDEA-042

# 2. Survey prior work
nk create literature "Fastembed vs Graph Traversal Survey" --board research --push
# → LIT-017

# 3. Formalize the hypothesis (quantitative target required)
nk create hypothesis "Fastembed improves entity retrieval by >=15% vs graph traversal" \
    --paper 131 --board research --push
# → H131.1 (auto-linked: H131.1 --tests--> PAPER-131)

# 4. Design the experiment
nk create experiment "Fastembed vs Graph Traversal A/B Study" \
    --hypothesis H131.1 --board research --push
# → EXPR-131.1 (auto-linked: EXPR-131.1 --validates--> H131.1)

# 5. Define the measure
nk create measure "Entity Retrieval Latency" \
    --unit milliseconds --category performance --board research --push
# → M-042
nk update M-042 --related EXPR-131.1  # Link measure to experiment
```

### GPU Experiment Queue

Experiments requiring GPU compute go through a NATS KV training queue:

```bash
# Queue a GPU job
nk training queue EXPR-131.1 \
    --being santiago-bahai \
    --corpus bahai \
    --machine DGX

# On DGX:
nk training claim --machine DGX
nk training complete TRAIN-001 --results research/shared/eval-data/expr1311/
nk training fail TRAIN-001 --error "OOM at epoch 3"
```

Queue metadata is stored as RDF triples in the experiment's Arrow record, making it
SPARQL-queryable:

```bash
nk query --sparql "SELECT ?label ?status WHERE { ?item a <https://nusy.dev/experiment/Experiment> . ?item <https://nusy.dev/experiment/runStatus> ?status . FILTER(?status = 'queued') }"
```

### HDD Diagnostics

```bash
nk hdd registry        # Full paper → hypothesis → experiment → measure chains
nk hdd validate        # Check for orphaned items
nk hdd validate --strict  # Fail CI on warnings
```

---

## CLI Reference (22 top-level + 17 subcommands)

**Core (8):** `create`, `move`, `update`, `comment`, `show`, `list`, `board`, `boards`
**Query (6):** `query`, `stats`, `history`, `roadmap`, `blocked`, `next`
**Planning (3):** `roadmap`, `critical-path`, `worklist`
**Management (6):** `validate`, `rank`, `export`, `next-id`, `migrate`, `init`
**HDD Research (8):** `hdd paper`, `hdd hypothesis`, `hdd experiment`, `hdd measure`,
  `hdd idea`, `hdd literature`, `hdd validate`, `hdd registry`
**Training Queue (5):** `training queue`, `training list`, `training claim`,
  `training complete`, `training fail`
**Graph-Native PRs (11):** `pr create`, `pr list`, `pr view`, `pr diff`, `pr review`,
  `pr merge`, `pr close`, `pr comment`, `pr checks`, `pr resolve`, `pr revise`

See [CLI-REFERENCE.md](CLI-REFERENCE.md) for full flag documentation.

---

## NATS Integration

When `--server nats://host:4222` is provided, all commands use a request-reply
pattern via NATS subjects:

| Subject pattern | Type | Purpose |
|----------------|------|---------|
| `kanban.cmd.{command}` | Request-reply | All CLI commands (create, move, list, ...) |
| `kanban.event.>` | Pub-sub (JetStream) | All mutation events (created, moved, deleted) |
| `training_queue` | NATS KV | Distributed GPU job queue |

### Event Payload Example

Every mutation emits a JetStream event:

```json
{
  "event_type": "kanban.item.moved",
  "timestamp": "2026-04-05T14:30:00.000Z",
  "source": "kanban-server",
  "payload": {
    "id": "EX-3001",
    "from": "backlog",
    "to": "in_progress",
    "agent": "Mini"
  },
  "correlation_id": "a1b2c3d4"
}
```

Subscribe once to `kanban.event.>` to receive all board activity:

```rust
// Rust — via noesis-ship
bus.subscribe("kanban.event.>", |event| {
    println!("{}: {:?}", event.event_type, event.payload);
    Box::pin(async {})
}).await?;
```

```python
# Python — via noesis-ship
await bus.subscribe("kanban.event.>", on_kanban_event)
```

---

## SHACL Shape Validation

All 13 item types have machine-readable SHACL shapes in Turtle (`.ttl`) format,
shipped inside the binary. Shapes define required fields, status enums, ID patterns,
and body section templates.

### ID Patterns

| Type | Pattern | Example |
|------|---------|---------|
| Expedition | `^EX-\d{4,}$` | `EX-3001` |
| Voyage | `^VY-\d{4,}$` | `VY-3001` |
| Hypothesis | `^H-\d{3,}$` | `H-131` |
| Experiment | `^EXPR-\d{3,}` | `EXPR-131.1` |
| Measure | `^M-\d{3,}$` | `M-042` |
| Paper | `^PAPER-\d{3,}$` | `PAPER-131` |

### Validate an Item (Python + rdflib)

```python
from rdflib import Graph, Namespace

KB = Namespace("https://nusy.dev/kanban/")

# Load an item graph from your Arrow store or .ttl file
item_graph = Graph()
item_graph.parse("my-item.ttl", format="turtle")

# Load the shapes
shapes_graph = Graph()
shapes_graph.parse("expedition.ttl", format="turtle")

# Run SHACL validation (requires pyshacl)
from pyshacl import validate
conforms, results_graph, results_text = validate(
    item_graph, shacl_graph=shapes_graph
)
print(results_text)
```

All shapes live at `ontology/shapes/` in the source tree. See
[EX-3667-SHACL-SHAPES.md](claude-workspace/EX-3667-SHACL-SHAPES.md) for the
full reference (600+ lines covering all 13 types, WIP constraints, SPARQL
validation examples, and status tables).

---

## Ecosystem

| Crate | Role |
|-------|------|
| [arrow-graph-git](https://crates.io/crates/arrow-graph-git) | Graph-native git primitives, WAL + atomic rename |
| **nusy-kanban** | Kanban engine + CLI (this crate) |
| [nusy-kanban-server](https://crates.io/crates/nusy-kanban) | NATS server for multi-agent coordination |

---

## Feature Flags

| Flag | Enables | Default |
|------|---------|---------|
| `client` | NATS client (async-nats + tokio) | on |
| `pr` | Graph-native PR workflows | on |
| `ci` | CI runner integration | on |
| `build` | Cranelift build/test integration | on |
| `codegraph` | Code graph integration | on |
| `fastembed` | Fastembed embedding backend | on |

**Note on dependencies:** This crate uses Arrow ecosystem crates from crates.io (`arrow`,
`parquet`, `nusy-arrow-git`, `nusy-graph-query`, `nusy-graph-review`). The internal
`nusy-arrow-core` crate is **not** a runtime dependency — it was removed in EX-3710
harmonization (previously declared but unused in source).

---

## Troubleshooting

### Build Errors

**`feature "client" references optional dependency "async-nats" but async-nats is not declared as optional`**
Ensure you have `optional = true` on both `async-nats` and `tokio` in `[dependencies]`.

**Missing Rust toolchain**
Requires Rust 1.75+ (edition 2021). Install via:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### NATS Connection Failures

**`Connection refused` when using `--server nats://...`**
- Verify the NATS server is running: `nc -zv 192.168.8.110 4222`
- Check server uptime on Mini: `ssh mini@192.168.8.110 uptime`
- Fall back to local mode (omit `--server` flag) — reads/writes `.nusy-kanban/` locally

**`Request timed out`**
Default timeout is 30s for commands. Check Mini's load and retry — transient overload.

### Schema Migration

When upgrading, Parquet files are auto-normalized:
- `persist.rs:normalize_batch()` appends null columns for new schema fields
- No manual migration needed — the store handles it transparently
- To force a full reload: delete `.nusy-kanban/*.parquet` and re-run commands

### Local vs Server Mode

| Flag | Behavior |
|------|----------|
| `--server nats://192.168.8.110:4222` | Single-writer to NATS-backed Arrow store on Mini |
| (none) | Local mode — reads/writes `.nusy-kanban/*.parquet` in current directory |

**Important:** Do not run two local-mode processes on the same directory simultaneously — Parquet writes are not atomic across files.

---

## Comparison

| Feature | nusy-kanban | Linear | GitHub Issues | Jira |
|---------|-------------|--------|---------------|------|
| Storage | Arrow/Parquet | Cloud DB | Cloud DB | Cloud DB |
| Offline-first | Yes | No | No | No |
| Multi-agent safe | NATS server | API | API | API |
| Query speed | Zero-copy columnar | API call | API call | API call |
| Research workflows | HDD board | No | No | No |
| Self-hosted | Yes (NATS) | No | GHES | Data Center |
| Crash safety | WAL + atomic rename | Managed | Managed | Managed |

---

## License

MIT
