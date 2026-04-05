# nusy-kanban CLI Reference

Arrow-native Rust kanban for the NuSy fleet. Tracks expeditions, chores, voyages,
hazards, signals, and HDD research items across dual boards (development + research).

## Quick Setup

```bash
# Required alias — all agents must set this at session start
alias nk='nusy-kanban --server nats://192.168.8.110:4222'
```

The `--server` flag connects to the nusy-kanban-server running on Mini, which
provides single-writer semantics. Using local mode (without `--server`) causes
ID collisions and store drift.

## Global Options

These flags apply to every command and must appear before the subcommand.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--root <PATH>` | path | `.` | Working directory (project root containing `.yurtle-kanban/`) |
| `--server <URL>` | URL | none | NATS server for remote single-writer access |

```bash
# Examples
nk --root /path/to/project board
nusy-kanban --server nats://192.168.8.110:4222 list
```

---

## 1. Core Work Commands

### create

Create a new work item. With `--push`, this is atomic: fetch latest, allocate ID,
create file in the correct subdirectory, commit, and push.

```
nk create <ITEM_TYPE> <TITLE> [OPTIONS]
```

| Argument / Flag | Type | Required | Default | Description |
|-----------------|------|----------|---------|-------------|
| `ITEM_TYPE` | enum | yes | -- | `expedition`, `chore`, `voyage`, `hazard`, `signal` |
| `TITLE` | string | yes | -- | Title for the new item |
| `--priority` | enum | no | `medium` | `low`, `medium`, `high`, `critical` |
| `--assign` | string | no | none | Agent name to assign |
| `--tags` | string | no | none | Comma-separated tags (e.g., `v14,rust,arrow`) |
| `--body` | string | no | none | Inline body content |
| `--body-file` | path | no | none | Read body content from a file |
| `--body-stdin` | flag | no | false | Read body content from stdin |
| `--template` | enum | no | none | Built-in template: `expedition`, `chore`, `voyage` |
| `--push` | flag | no | false | Atomic create + git commit + push |

```bash
# Create an expedition and push atomically (recommended)
nk create expedition "Arrow schema validation" --push

# Create a chore with priority and assignment
nk create chore "Update CI pipeline" --priority high --assign Mini --push

# Create from a template with tags
nk create expedition "New graph primitive" --template expedition --tags v14,rust --push
```

### move

Change an item's status. Optionally reassign at the same time.

```
nk move <ID> <STATUS> [OPTIONS]
```

| Argument / Flag | Type | Required | Default | Description |
|-----------------|------|----------|---------|-------------|
| `ID` | string | yes | -- | Item ID (e.g., `EXP-3042`) |
| `STATUS` | string | yes | -- | Target status (see Board States below) |
| `--assign` | string | no | none | Reassign to an agent |
| `--force` | flag | no | false | Bypass WIP limits (audited — include rationale in commit) |

```bash
# Start work on an expedition
nk move EXP-3042 in_progress --assign Mini

# Force-move past WIP limit (requires justification)
nk move EXP-3043 in_progress --force
```

### update

Modify fields on an existing item without changing its status.

```
nk update <ID> [OPTIONS]
```

| Argument / Flag | Type | Required | Default | Description |
|-----------------|------|----------|---------|-------------|
| `ID` | string | yes | -- | Item ID |
| `--title` | string | no | none | New title |
| `--priority` | enum | no | none | `low`, `medium`, `high`, `critical` |
| `--assign` | string | no | none | Reassign to agent |
| `--tags` | string | no | none | Replace tags (comma-separated) |
| `--body` | string | no | none | Replace body content |
| `--body-file` | path | no | none | Replace body from file |
| `--related` | string | no | none | Comma-separated related IDs (e.g., `VOY-3001,EXP-3010`) |
| `--depends-on` | string | no | none | Comma-separated dependency IDs |

```bash
# Update priority and add tags
nk update EXP-3042 --priority critical --tags v14,blocked

# Link to a voyage
nk update EXP-3042 --related VOY-3005
```

### comment

Add a comment to an item's history.

```
nk comment <ID> <TEXT>
```

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `ID` | string | yes | Item ID |
| `TEXT` | string | yes | Comment text |

```bash
nk comment EXP-3042 "Blocked on Arrow schema changes in nusy-arrow-core"
```

### show

Display full details for an item.

```
nk show <ID> [OPTIONS]
```

| Argument / Flag | Type | Required | Default | Description |
|-----------------|------|----------|---------|-------------|
| `ID` | string | yes | -- | Item ID |
| `--format` | enum | no | `default` | `default` (terminal), `md` (markdown), `json` |

```bash
nk show EXP-3042
nk show EXP-3042 --format json
```

### list

List items with optional filters.

```
nk list [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--status` | string | no | none | Filter by status (e.g., `in_progress`, `backlog`) |
| `--board` | enum | no | `development` | `development` or `research` |
| `--item-type` | string | no | none | Filter by type (e.g., `expedition`, `chore`, `hypothesis`) |
| `--assignee` | string | no | none | Filter by assigned agent |

```bash
# List all in-progress work
nk list --status in_progress

# List research hypotheses
nk list --board research --item-type hypothesis

# List everything assigned to Mini
nk list --assignee Mini
```

### board

Display the kanban board in the terminal.

```
nk board [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--board` | enum | no | `development` | `development` or `research` |

```bash
nk board
nk board --board research
```

### boards

List all configured boards.

```
nk boards
```

No flags. Reads from `.yurtle-kanban/config.yaml` and displays board names,
directories, and item types.

```bash
nk boards
```

---

## 2. Query & Analytics

### query

Search across items using natural language, text matching, or SPARQL-like filters.

```
nk query [QUERY...] [OPTIONS]
```

| Argument / Flag | Type | Required | Default | Description |
|-----------------|------|----------|---------|-------------|
| `QUERY` | string | no | none | Positional natural language query |
| `--search` | string | no | none | Semantic text search |
| `--sparql` | string | no | none | SPARQL-like filter query |
| `--json` | flag | no | false | Output as JSON |
| `--verbose` | flag | no | false | Include full item bodies in results |
| `--no-semantic` | flag | no | false | Disable semantic ranking, use text match only |
| `--top` | integer | no | 20 | Maximum number of results |

```bash
# Natural language query
nk query "Arrow schema validation"

# SPARQL query for queued experiments
nk query --sparql "SELECT ?label WHERE { ?item a <https://nusy.dev/experiment/Experiment> . ?item <https://nusy.dev/experiment/runStatus> 'queued' . OPTIONAL { ?item <http://www.w3.org/2000/01/rdf-schema#label> ?label } }"

# Text search with JSON output
nk query --search "git primitives" --json --top 5
```

### stats

Display board statistics: item counts by status, type, and assignee.

```
nk stats
```

No flags. Shows summary for the default (development) board.

```bash
nk stats
```

### history

Show recent activity across the board.

```
nk history [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--week` | flag | no | false | Limit to the current week only |

```bash
nk history
nk history --week
```

### roadmap

Voyage-grouped, dependency-ordered view of all work. Server-side computation on Arrow RecordBatches.

```
nk roadmap [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--flat` | flag | no | false | Flat backlog ranked by priority (legacy view) |
| `--ready` | flag | no | false | Show only items with all dependencies met |

```bash
nk roadmap                # Voyage-grouped with progress bars
nk roadmap --flat         # Flat priority-ranked backlog
nk roadmap --ready        # Only unblocked items
```

### critical-path

Show the dependency chain with parallel tracks and depth levels. Identifies the longest dependency chain and items that can run concurrently.

```
nk critical-path
```

No flags. Computes topological sort, depth levels, and parallel groups from `depends_on` edges.

```bash
nk critical-path
```

### worklist

Show agent work assignments based on dependency readiness and current assignments.

```
nk worklist [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--agents` | string | no | `DGX,M5,Mini` | Comma-separated agent names |
| `--depth` | int | no | 3 | Items per agent |

```bash
nk worklist                                         # Default 3 agents, 3 deep
nk worklist --agents "DGX,M5,Mini,negaDGX" --depth 5
```

### blocked

Show all items that are currently blocked by unresolved dependencies.

```
nk blocked
```

No flags. Scans `depends_on` fields and reports items whose dependencies are not `done`.

```bash
nk blocked
```

### next

Suggest the next item to work on, based on priority and dependencies.

```
nk next [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--assignee` | string | no | none | Filter suggestions for a specific agent |

```bash
nk next
nk next --assignee Mini
```

---

## 3. Management

### validate

Check all kanban files for YAML frontmatter correctness, required fields,
and board configuration consistency.

```
nk validate
```

No flags. Reports errors and warnings.

```bash
nk validate
```

### rank

Set a numeric rank on an item for manual priority ordering.

```
nk rank <ID> <RANK>
```

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `ID` | string | yes | Item ID |
| `RANK` | integer | yes | Numeric rank (lower = higher priority) |

```bash
nk rank EXP-3042 1
```

### export

Export board data in various formats.

```
nk export [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--id` | string | no | none | Export a single item by ID |
| `--format` | enum | no | `item` | `expedition-index`, `markdown`, `json`, `item` |
| `--board` | enum | no | `development` | `development` or `research` |
| `--output` | path | no | stdout | Write to file instead of stdout |

```bash
# Export full board as JSON
nk export --format json --output board.json

# Export a single item
nk export --id EXP-3042 --format item
```

### next-id

Allocate and return the next available ID for a given item type. Multi-agent safe.

```
nk next-id <ITEM_TYPE>
```

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `ITEM_TYPE` | enum | yes | `expedition`, `chore`, `voyage`, `hazard`, `signal` |

```bash
nk next-id expedition
# → EXP-3087
```

> **Note:** Prefer `nk create ... --push` which allocates the ID atomically.
> Use `next-id` only when you need the ID before creating (rare).

### migrate

Migrate existing markdown kanban files into the Arrow store.

```
nk migrate
```

No flags. Reads all `.md` files from board directories, parses frontmatter,
and writes them into the Arrow-native store.

```bash
nk migrate
```

### init

Initialize a new nusy-kanban project in the current directory. Creates the
`.yurtle-kanban/` config directory and default board configuration.

```
nk init
```

No flags.

```bash
nk init
```

---

## 4. HDD Research Board

All research commands live under the `nk hdd` subcommand. They operate on the
research board (`research/` directory).

### hdd paper

Create a new research paper item.

```
nk hdd paper <TITLE>
```

```bash
nk hdd paper "Neurosymbolic Curriculum Transfer"
```

### hdd hypothesis

Create a new hypothesis.

```
nk hdd hypothesis <TITLE>
```

```bash
nk hdd hypothesis "Y-layer depth correlates with reasoning accuracy"
```

### hdd experiment

Create a new experiment.

```
nk hdd experiment <TITLE>
```

```bash
nk hdd experiment "Three-way A/B/C eval of consolidation strategies"
```

### hdd measure

Create a new measure definition.

```
nk hdd measure <TITLE>
```

```bash
nk hdd measure "ACF coherence score"
```

### hdd idea

Capture a new idea.

```
nk hdd idea <TITLE>
```

```bash
nk hdd idea "Use graph attention for Y2 rule weighting"
```

### hdd literature

Create a new literature review item.

```
nk hdd literature <TITLE>
```

```bash
nk hdd literature "Survey of neurosymbolic reasoning benchmarks"
```

### hdd validate

Validate research board items for schema conformance.

```
nk hdd validate
```

### hdd registry

Display the research item registry (all papers, hypotheses, experiments, etc.).

```
nk hdd registry
```

---

## 5. Graph-Native PRs

All PR commands live under the `nk pr` subcommand. These operate on graph-native
pull requests stored in the Arrow knowledge graph (not GitHub PRs).

### pr create

Create a new graph-native PR.

```
nk pr create [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--title` | string | yes | -- | PR title |
| `--base` | string | no | `main` | Base branch |
| `--body` | string | no | none | PR description |

```bash
nk pr create --title "Add namespace partitioning" --base main --body "Implements EXP-3042"
```

### pr list

List open graph-native PRs.

```
nk pr list
```

### pr view

View details of a specific PR.

```
nk pr view <ID>
```

```bash
nk pr view 7
```

### pr diff

Show the diff for a PR.

```
nk pr diff <ID>
```

```bash
nk pr diff 7
```

### pr review

Submit a review on a PR.

```
nk pr review <ID> [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--approve` | flag | no | false | Approve the PR |
| `--request-changes` | flag | no | false | Request changes |
| `--body` | string | no | none | Review comment |
| `--reviewer` | string | no | none | Reviewer name |

```bash
nk pr review 7 --approve --body "LGTM" --reviewer Mini
nk pr review 7 --request-changes --body "Missing tests for edge case" --reviewer M5
```

### pr merge

Merge a PR.

```
nk pr merge <ID> [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--delete-branch` | flag | no | false | Delete the source branch after merge |

```bash
nk pr merge 7 --delete-branch
```

### pr close

Close a PR without merging.

```
nk pr close <ID>
```

```bash
nk pr close 7
```

### pr comment

Add a comment to a PR.

```
nk pr comment <ID> [OPTIONS]
```

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--body` | string | yes | -- | Comment text |

```bash
nk pr comment 7 --body "Needs rebase on main"
```

### pr checks

Show status checks for a PR.

```
nk pr checks <ID>
```

```bash
nk pr checks PROP-2057
```

### pr resolve

Resolve a review comment (unblocks approval — unresolved comments block `pr review --approve`).

```
nk pr resolve <ID> --comment-id <CMT-ID>
```

```bash
nk pr resolve PROP-2057 --comment-id CMT-038
```

### pr revise

Re-enter review after rejection. Anyone can run this (no author restriction). Moves proposal from `rejected` → `revised` → `reviewing`.

```
nk pr revise <ID>
```

```bash
nk pr revise PROP-2057
```

**Full rejection recovery flow:**
```bash
# 1. Fix the code, push
# 2. Re-enter review
nk pr revise PROP-2057
# 3. Resolve each comment
nk pr resolve PROP-2057 --comment-id CMT-038
# 4. Approve
nk pr review PROP-2057 --approve --reviewer "M5"
```

---

## ID Formats

IDs are allocated per-type with a namespace starting offset.

| Prefix | Type | Starting ID | Board |
|--------|------|-------------|-------|
| `EXP-` | Expedition | 3001+ | development |
| `CHORE-` | Chore | 3001+ | development |
| `VOY-` | Voyage | 3001+ | development |
| `HAZ-` | Hazard | 3001+ | development |
| `SIG-` | Signal | 3001+ | development |
| `PROP-` | Proposal | 2001+ | development |
| `PAPER-` | Paper | 3001+ | research |
| `H-` | Hypothesis | 3001+ | research |
| `EXPR-` | Experiment | 3001+ | research |
| `M-` | Measure | 3001+ | research |
| `IDEA-` | Idea | 3001+ | research |
| `LIT-` | Literature | 3001+ | research |

> **Note:** Items created before the Arrow migration (nusy-kanban) retain their
> original IDs (e.g., `EXP-872`). The 3001+ namespace applies to items created
> by nusy-kanban after the migration cutover.

---

## Board States

### Development Board

All development item types share a common lifecycle:

```
backlog --> in_progress --> review --> done
                                  \-> abandoned
```

| Status | Description |
|--------|-------------|
| `backlog` | Queued, not yet started |
| `in_progress` | Actively being worked on |
| `review` | PR open, awaiting review |
| `done` | Merged and complete |
| `abandoned` | Cancelled or superseded |

### Research Board (Per-Type Lifecycles)

Research items have type-specific state machines:

**Hypothesis:**
```
draft --> active --> retired
```
Hypotheses are never "complete" — experiments validate them per-version.

**Measure:**
```
draft --> active --> retired
```
ACF measures stay `active` indefinitely.

**Paper:**
```
draft --> outline --> writing --> review --> complete
                                        \-> abandoned
```

**Experiment:**
```
planned --> running --> complete
                   \-> abandoned
```
One-shot, version-bound.

**Literature:**
```
draft --> active --> complete
```

**Idea:**
```
captured --> formalized
         \-> abandoned
```
Promoted to hypothesis when formalized.

---

## Configuration

Board configuration lives in `.yurtle-kanban/config.yaml` at the project root.
It defines board names, directories, item types, and state machines. Run `nk boards`
to inspect the current configuration.

---

## Common Workflows

### Start a new expedition (recommended)

```bash
# 1. Create on main atomically
nk create expedition "Implement graph merging" --push

# 2. Create feature branch
git checkout -b exp-3087-graph-merging

# 3. Do work, commit, push branch
# 4. Open PR via gh
gh pr create --title "EXP-3087: Implement graph merging" --body "..."

# 5. Move to review
nk move EXP-3087 review
```

### Pick up existing work

```bash
# 1. Check what's available
nk next --assignee Mini

# 2. Assign and start
nk move EXP-3042 in_progress --assign Mini

# 3. Create branch and work
git checkout -b exp-3042-schema-validation
```

### Check fleet status

```bash
nk board                          # Development board overview
nk board --board research         # Research board overview
nk list --status in_progress      # Who's working on what
nk stats                          # Summary counts
nk blocked                        # What's stuck
```
