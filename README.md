# nusy-kanban

[![Crates.io](https://img.shields.io/crates/v/nusy-kanban)](https://crates.io/crates/nusy-kanban)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Arrow-native kanban with a nautical soul** — track expeditions, voyages, and
research with graph-native PRs, dual boards, and NATS-powered multi-agent
collaboration.

## For AI Developers

Install the CLI (no features, pure kanban):

```bash
cargo install nusy-kanban --no-default-features
```

Initialize a local store in any git repo:

```bash
nk init                    # creates .nusy-kanban/ with Arrow store
nk create expedition "Build something great" --body "Phase 1: ..." --push
nk board                   # see the board
nk move EXP-3001 underway  # move to in_progress
nk show EXP-3001           # full detail
nk list --status harbor    # backlog items
```

For multi-agent teams, point everyone at the same NATS server:

```bash
alias nk='nusy-kanban --server nats://your-host:4222'
```

## Architecture

```
                       nusy-kanban CLI
┌──────────────────────────────────────────────────────────┐
│  Arrow RecordBatch  │  NATS client  │  Shell alias       │
└──────────┬──────────┴───────┬───────┴────────────────────┘
           │                  │
           ▼                  ▼
┌───────────────────┐  ┌─────────────────────┐
│  Parquet snapshot │  │   NATS server       │
│  (nusy-arrow-git) │  │  (single-writer KV) │
└───────────────────┘  └─────────────────────┘
```

- **Storage:** Apache Arrow RecordBatches persisted to Parquet via WAL + atomic rename
- **Multi-agent:** NATS server provides single-writer semantics — no ID collisions, no store drift
- **Queries:** Zero-copy columnar scans over Arrow data — fast even at 10k+ items

## Motivation

File-based kanban tools break under pressure. YAML parsing is fragile, git merge
conflicts pile up on concurrent access, and querying across hundreds of items
means globbing directories and hoping for the best.

`nusy-kanban` replaces all of that with Apache Arrow RecordBatches and Parquet
persistence. Queries are zero-copy scans over columnar data. Writes go through a
crash-safe WAL with atomic rename. Multi-agent teams coordinate through a NATS
server that provides single-writer semantics — no more ID collisions or store
drift when three developers push at once.

The result: a kanban engine that handles thousands of work items at the speed of
an in-memory database, persists to a single Parquet file, and still feels like
typing `git status`.

## The Nautical Theme

Every project is a voyage. Work items follow a nautical lifecycle:

```
Harbor → Provisioning → Underway → Approaching Port → Arrived
(backlog)   (ready)    (in_progress)   (review)       (done)
```

| Type | Meaning | Example |
|------|---------|---------|
| **Expedition** (EXP) | Feature work | "Add OAuth2 provider support" |
| **Chore** (CHORE) | Maintenance | "Update CI to Rust 1.82" |
| **Voyage** (VOY) | Multi-phase campaign | "V14 Arrow Migration" |
| **Hazard** (HAZ) | Risk or blocker | "NATS reconnect drops events" |
| **Signal** (SIG) | Observation | "Parquet write latency spike at 10k rows" |

## Features

**Core**
- Create, move, update, comment, and delete work items
- Atomic `create --push` (allocate ID + create + git commit + push in one command)
- Full status history with timestamps and assignee tracking
- WIP limits with `--force` override and audit trail
- Parquet persistence with WAL + atomic rename (via nusy-arrow-git)
- ID allocation starting at 3001+ (clean separation from legacy file-era IDs)
- YAML frontmatter import/export for interop with existing markdown workflows
- Shell alias friendly (`alias nk='nusy-kanban --server nats://...'`)

**Query & Visibility**
- `board` — columnar board view with status counts
- `list` — filter by status, type, assignee, tags, board
- `show` — full item detail with comments and status history
- `query` — natural-language search across all items
- `stats` — board statistics and velocity metrics
- `history` — audit log for any item

**Planning & Analysis**
- `roadmap` — voyage-grouped, dependency-ordered view (`--flat` for priority-ranked, `--ready` for unblocked only)
- `critical-path` — dependency chain with parallel tracks and depth levels
- `worklist` — agent work assignments based on dependency readiness (`--agents`, `--depth`)
- `blocked` — surface items with unresolved dependency blockers
- `list --ready` — filter any list to only items with all dependencies met

**Management**
- `validate` — check board integrity (orphaned refs, missing fields)
- `export` — dump board to JSON, CSV, or Parquet
- `rank` / `next` — priority ranking and "what should I work on?" recommendations
- `migrate` — upgrade from file-based kanban stores

**HDD Research Board**
- Dedicated research board with domain-specific types: Paper, Hypothesis, Experiment, Measure, Idea, Literature
- Per-type lifecycles (hypotheses are `draft → active → retired`, never "complete")
- `hdd` subcommand for Hypothesis-Driven Development workflows
- SPARQL-style queries over experiment metadata (run status, blockers, GPU requirements)
- Experiment queue tracking with `expr:runStatus` predicates
- Cross-board linking between expeditions and experiments
- Shared eval-data conventions for multi-agent research
- Research board stats separate from development velocity

**Graph-Native PR Review** (9 capabilities via `nk pr`)
- `pr create` / `pr list` / `pr show` / `pr approve` / `pr merge`
- Safety gates: blocks merge if tests fail or required reviewers haven't approved
- Proposal diffs rendered as graph deltas (what changed in the knowledge graph)
- Cross-agent review assignment
- Integrates with nusy-graph-review for structured proposal workflows

## Quick Start

```bash
cargo install nusy-kanban

# Initialize a new kanban store in the current repo
nusy-kanban init

# Create your first expedition (atomically commits and pushes)
nk create expedition "My First Feature" \
  --body "Phase 1: Design the API. Phase 2: Implement handlers." \
  --push

# Start working on it
nk move EXP-3001 in_progress --assign "dev"

# See the board
nk board
```

```
Development Board
─────────────────────────────────────────────────────
Harbor (1)  │ Underway (1)    │ Arrived (0)
            │                 │
            │ EXP-3001        │
            │  My First Feat… │
            │  @dev           │
─────────────────────────────────────────────────────
```

## Multi-Agent Setup (NATS)

For teams with multiple developers or AI agents working concurrently, point
everyone at a shared NATS server:

```bash
alias nk='nusy-kanban --server nats://your-host:4222'
```

The server provides single-writer semantics — ID allocation, status transitions,
and WIP enforcement are all serialized. No lock files, no merge conflicts. See
[NATS-SERVER.md](NATS-SERVER.md) for setup instructions.

## Dual Boards

`nusy-kanban` runs two boards from the same store:

| Board | Directory | Types | Lifecycle |
|-------|-----------|-------|-----------|
| **Development** | `kanban-work/` | Expedition, Chore, Voyage, Hazard, Signal | Nautical (Harbor → Arrived) |
| **Research** | `research/` | Paper, Hypothesis, Experiment, Measure, Idea, Literature | Per-type (see below) |

Research items follow domain-appropriate lifecycles rather than a single pipeline:

- **Hypothesis**: `draft → active → retired` (validated by experiments, never "complete")
- **Experiment**: `planned → running → complete / abandoned` (one-shot, version-bound)
- **Paper**: `draft → outline → writing → review → complete / abandoned`
- **Measure**: `draft → active → retired` (long-lived metrics stay active)
- **Idea**: `captured → formalized / abandoned` (promoted to hypothesis)
- **Literature**: `draft → active → complete`

```bash
nk list --board research --type hypothesis --status active
nk hdd status   # HDD dashboard across all research types
```

## Comparison

| Feature | nusy-kanban | Linear | GitHub Issues | Jira | Plain files |
|---------|-------------|--------|---------------|------|-------------|
| Storage | Arrow/Parquet | Cloud DB | Cloud DB | Cloud DB | Markdown/YAML |
| Offline-first | Yes | No | No | No | Yes |
| Multi-agent safe | NATS server | API | API | API | Git (conflicts) |
| Query speed | Zero-copy columnar | API call | API call | API call | grep/glob |
| Research workflows | HDD board | No | No | No | No |
| Graph-native PRs | Safety gates + Y-layer | No | GitHub PRs | No | No |
| Self-hosted | Yes (NATS) | No | GHES | Data Center | Yes (git) |
| Crash safety | WAL + atomic rename | Managed | Managed | Managed | None |

## Commands (22 top-level + 17 subcommands)

**Core (8):** `create`, `move`, `update`, `comment`, `show`, `list`, `board`, `boards`
**Query (6):** `query`, `stats`, `history`, `roadmap`, `blocked`, `next`
**Planning (3):** `roadmap`, `critical-path`, `worklist`
**Management (6):** `validate`, `rank`, `export`, `next-id`, `migrate`, `init`
**HDD Research (8):** `hdd paper`, `hdd hypothesis`, `hdd experiment`, `hdd measure`, `hdd idea`, `hdd literature`, `hdd validate`, `hdd registry`
**Graph-Native PRs (11):** `pr create`, `pr list`, `pr view`, `pr diff`, `pr review`, `pr merge`, `pr close`, `pr comment`, `pr checks`, `pr resolve`, `pr revise`

See [CLI-REFERENCE.md](CLI-REFERENCE.md) for full flag documentation and examples.

## License

MIT
