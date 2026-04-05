# HDD Usage Guide for AI Developers

**For AI developers building with the nusy-kanban FOSS crate.**

HDD (Hypothesis-Driven Development) is a research methodology that applies the rigor of
test-driven development to scientific investigation. Where TDD writes a failing test before
writing code, HDD writes a falsifiable hypothesis before running an experiment. Where BDD
defines behavior scenarios, HDD defines quantitative targets. The output of TDD is working
code; the output of HDD is a validated (or refuted) claim backed by data.

HDD synthesizes pre-registration (Nosek et al., 2018), design science research (Hevner et al.,
2004), and test-driven development (Beck, 2003) into a toolchain-integrated workflow for
autonomous AI research. It was developed for the NuSy neurosymbolic AI platform, where AI
"beings" learn, reason, and work autonomously, but the methodology is domain-agnostic.

The key insight: **only validated enhancements ship**. We don't merge code that we can't prove
works. This keeps the codebase clean and ensures every feature has evidence behind it.

## HDD vs TDD vs BDD

| Aspect | TDD | BDD | HDD |
|--------|-----|-----|-----|
| Unit of work | Function | Behavior | Hypothesis |
| Test | Unit test | Scenario | Experiment |
| Pass criteria | Assert | Gherkin | Quantitative target |
| Output | Working code | Working feature | Validated claim |
| Discovery phase | N/A | N/A | Literature review |
| Negative result | Bug | Regression | Valid science |
| Documentation | Tests | Specs | Paper |

## The 6-Phase Cycle

```
IDEA → LITERATURE → HYPOTHESIS → EXPERIMENT → ANALYSIS → PAPER
                                                  ↓
                                          FAIL? → Refine → loop
```

Every phase has a defined artifact. Every artifact has a machine-readable ID and
cross-links to its neighbors in the chain. The cycle is short — IDEA to PAPER should
take days, not months.

---

## The 6 Research Types

All research types live on the **research board**. Create them with `nk` commands;
they write directly to the Arrow store via NATS — no files required.

### 1. Paper

**ID:** `PAPER-{N}` (e.g., `PAPER-131`)

Papers are the terminal artifact — the publication that documents validated hypotheses.
A paper contains multiple hypotheses, each with its own experiment chain.

**Auto-links:** None (papers are the root of a chain).

**Create:**
```bash
nk create paper "Symbolic-First Perception in V14.2" --board research --push
```

**CLI create:**
```bash
nusy-kanban hdd paper create 131 "Symbolic-First Perception" --push
```

---

### 2. Hypothesis

**ID:** `H{paper}.{seq}` (e.g., `H131.1`, `H131.2`) for paper-scoped;
or `H-{N}` for standalone

A falsifiable claim with a quantitative target. One paper can have multiple hypotheses.

**Auto-links:**
- Hypothesis **tests** Paper via the `kb:tests` predicate
- The `kb:tests` relation is created automatically when you create the hypothesis

**Create:**
```bash
# Paper-scoped (recommended): links to PAPER-131 automatically
nk create hypothesis "Fastembed outperforms graph traversal for entity retrieval by >=15%" \
    --paper 131 --board research --push
```

**CLI create:**
```bash
nusy-kanban hdd hypothesis create "Fastembed outperforms graph traversal" \
    --paper 131 --target ">=15%" --push
```

**Hypothesis checklist:**
- Falsifiable — there exists a result that would refute it
- Quantitative — has a measurable target (e.g., ">=15%", ">=85% accuracy")
- Specific — names the mechanism, not just the outcome
- One claim per hypothesis (not a compound statement)

**Example:** `H131.1: Fastembed outperforms graph traversal by >=15% on entity retrieval latency`

---

### 3. Experiment

**ID:** `EXPR-{paper}.{seq}` (e.g., `EXPR-131.1`) or `EXPR-{N}` (standalone)

A reproducible protocol — formal enough to be reproduced exactly and to have its
Method/Results sections pulled directly into a paper.

**Auto-links:**
- Experiment **validates** Hypothesis via the `kb:validates` predicate
- The hypothesis must exist before the experiment can be created

**Create:**
```bash
nk create experiment "Fastembed vs Graph Traversal A/B Study" \
    --hypothesis H131.1 --board research --push
```

**CLI create:**
```bash
nusy-kanban hdd experiment create EXPR-131 --hypothesis H131.1 \
    --title "Fastembed vs Graph Traversal A/B Study" --push
```

**Minimum viable experiment protocol:**
```
# EXPR-131.1: Fastembed vs Graph Traversal A/B Study

## Purpose
Does fastembed achieve >=15% lower latency than graph traversal for entity retrieval?

## Pre-Registration
- Hypothesis: H131.1
- Primary measure: M-042 (Entity Retrieval Latency)
- Target: >=15% improvement
- Locked: [git commit hash] on [date]

## Hypotheses Tested
| ID | Statement | Target | Status |

## Method
### Participants (Beings)
| Being | Version | Role |

### Procedure
1. Step-by-step protocol

## Results
| Measure | Target | Actual | Status |

## Data Location
| Data | Path |
```

---

### 4. Measure

**ID:** `M-{N}` (e.g., `M-042`)

A quantitative metric used to determine pass/fail for an experiment. Measures are
defined once and reused across experiments. Every hypothesis needs at least one
measure — the metric that determines whether the target was met.

**Auto-links:**
- Measure **measures** Experiment via the `kb:measures` predicate
- Can be created standalone (unlinked) or linked to a specific experiment

**Create:**
```bash
nk create measure "Entity Retrieval Latency" --board research --push
```

**CLI create:**
```bash
nusy-kanban hdd measure create "Entity Retrieval Latency" \
    --unit milliseconds --category performance --push
```

**Link a measure to an experiment after creation:**
```bash
nk update M-042 --related EXPR-131.1
```

**Measure categories:**

| Category | What it measures | Examples |
|----------|-----------------|----------|
| accuracy | Correctness of output | Entity disambiguation accuracy |
| performance | Speed/efficiency | Training throughput, inference latency |
| ylayer_population | Knowledge graph structure | Y5 procedure count, Y3 relationship density |
| quality | Richness/depth of output | Semantic richness, confidence calibration |
| autonomy | Independence of being | Task autonomy score, escalation rate |

---

### 5. Idea

**ID:** `IDEA-{N}` (e.g., `IDEA-042`)

A raw research question or observation — captured without filtering. The starting
point of the HDD cycle. Ideas are not yet testable; they graduate to hypotheses
once a measure and target are identified.

**Auto-links:** None (ideas are standalone capture).

**Create:**
```bash
nk create idea "Would fastembed improve entity retrieval latency?" \
    --board research --push
```

**CLI create:**
```bash
nusy-kanban hdd idea create "Would fastembed improve entity retrieval latency?" --push
```

**Tip:** Use the `--tags` flag to mark the domain (e.g., `--tags "perception,v14.2"`).

---

### 6. Literature

**ID:** `LIT-{N}` (e.g., `LIT-017`)

A survey of prior work on a topic. HDD explicitly embraces LLM-assisted literature
review as a first-class research tool. Literature files document frameworks,
standards, and prior approaches that inform hypothesis formation.

**Auto-links:** None (literature is reference material).

**Create:**
```bash
nk create literature "Fastembed and Graph Traversal for Entity Retrieval Survey" \
    --board research --push
```

**CLI create:**
```bash
nusy-kanban hdd literature create "Fastembed and Graph Traversal Survey" --push
```

**LLM-assisted literature review pattern:**

```bash
# Ask the LLM: "Is there prior work on [your question]?"
# Ask: "What frameworks or standards exist for [your domain]?"
# Ask: "What metrics are used for [your outcome]?"
# Document findings in the Literature item body
```

---

## The Research Chain

Each type links to its neighbors via typed RDF predicates:

```
PAPER-131 ──tests──> H131.1 ──validates──> EXPR-131.1 ──measures──> M-042
                                    ↑
                               (experiment linked to EXP-872)
```

| From | To | Predicate | Direction |
|------|----|-----------|-----------|
| Hypothesis | Paper | `kb:tests` | Hyp tests Paper |
| Experiment | Hypothesis | `kb:validates` | Expr validates Hyp |
| Measure | Experiment | `kb:measures` | Measure measures Expr |

---

## For AI Developers

### Using `nk hdd` subcommands

The `nk hdd` command provides direct HDD workflow support:

```bash
nk hdd registry        # Show paper → hypothesis → experiment → measure chains
nk hdd validate         # Check for orphaned hypotheses or experiments
nk hdd validate --strict  # Warnings treated as errors (for CI)
```

```bash
# View the full research chain for a paper
nk hdd registry

# Output:
# PAPER-131: Symbolic-First Perception
#   H131.1: Fastembed outperforms graph traversal by >=15%
#     EXPR-131.1: Fastembed vs Graph Traversal A/B Study
#       M-042: Entity Retrieval Latency
#   H131.2: [next hypothesis]
#     ...
```

### How a Being Tracks Its Own Experiments

A being can follow the HDD cycle autonomously:

1. **Observe** — Being notices a performance gap or failure mode during operation
2. **Capture** — Create an Idea: `nk create idea "Observation description" --push`
3. **Research** — Create Literature item and document prior approaches
4. **Formalize** — Graduate idea to Hypothesis with a quantitative target
5. **Design** — Create Experiment with a reproducible protocol and pre-registered target
6. **Execute** — Run the experiment, collect data
7. **Analyze** — Compare results to target; record VALIDATED or NOT SUPPORTED

**Example — being Santiago notices poor recall:**

```bash
# Santiago (an AI being) observes: "v14.2 has poor recall on entity queries"
# Step 1: Capture the idea
nk create idea "v14.2 entity recall is poor — graph traversal may be slower than fastembed" \
    --tags "perception,v14.2" --board research --push
# Returns: IDEA-042

# Step 2: Survey prior work (LLM-assisted)
nk create literature "Fastembed vs Graph Traversal for Entity Retrieval" \
    --board research --push
# Returns: LIT-017

# Step 3: Formalize the hypothesis (quantitative target required)
nk create hypothesis "Fastembed improves entity recall by >=15% vs graph traversal" \
    --paper 131 --board research --push
# Returns: H131.1 (auto-linked to PAPER-131)

# Step 4: Design the experiment
nk create experiment "Fastembed vs Graph Traversal A/B Study" \
    --hypothesis H131.1 --board research --push
# Returns: EXPR-131.1 (auto-linked to H131.1)

# Step 5: Define the measure
nk create measure "Entity Retrieval Latency" \
    --unit milliseconds --category performance --board research --push
# Returns: M-042
nk update M-042 --related EXPR-131.1  # Link measure to experiment
```

### The Experiment Queue: `nk training` Workflow

Experiments that need GPU compute (training or evaluation) go through the
training queue via NATS KV:

```bash
# Queue a GPU job — experiment runs on DGX
nk training queue EXPR-131.1 \
    --being santiago-bahai \
    --corpus bahai \
    --machine DGX

# DGX checks the queue at session start
nk training list --status queued

# DGX claims the next job
nk training claim --machine DGX

# DGX runs the experiment, then marks it complete
nk training complete TRAIN-001 --results research/shared/eval-data/expr1311/

# If it fails
nk training fail TRAIN-001 --error "OOM at epoch 3"
```

**Queue predicates** (stored in the experiment's RDF block):

| Predicate | Values | Purpose |
|-----------|--------|---------|
| `expr:runStatus` | `queued`, `running`, `done` | Current state |
| `expr:blockedBy` | `EXP-872`, etc. | What must complete first |
| `expr:runOn` | `"DGX"` | Target machine |
| `expr:requiresGPU` | `"true"^^xsd:boolean` | GPU requirement |
| `expr:estimatedMinutes` | integer | Expected runtime |

### Linking Research to Development Expeditions

Research experiments validate development work. Use the `related` field to link
an experiment to the expedition that implements it:

```bash
# EXP-872 is the development expedition for the fastembed feature
nk update EXPR-131.1 --related EXP-872
```

This creates a cross-board trace:

```
EXP-872 (development) ──implements──> EXPR-131.1 (research)
```

Query across the full chain:

```bash
# Find all experiments for a paper
nk query "experiments for PAPER-131"

# Find all expeditions linked to a research experiment
nk query "expeditions for EXPR-131.1"
```

---

## Worked Example: Santiago Tracks Entity Recall

This is a complete scenario following the full HDD cycle.

**Setup:** Being Santiago is running v14.2 and notices entity queries are slow.

---

**Step 1: Observe and Capture**

Santiago creates an idea:

```bash
nk create idea "v14.2 entity recall is poor — fastembed might outperform graph traversal" \
    --tags "perception,v14.2" --board research --push
# Returns: IDEA-042
```

---

**Step 2: Literature Review**

Santiago surveys prior work:

```bash
nk create literature "Fastembed and Graph Traversal for Entity Retrieval Survey" \
    --board research --push
# Returns: LIT-017
```

Santiago adds findings to the Literature item body:
- Fastembed (NBD-Team): approximate nearest neighbor search via graph traversal of compressed embeddings
- BM25: traditional sparse retrieval, no semantic understanding
- Prior NuSy work: graph traversal was default since v12

---

**Step 3: Formalize Hypothesis**

Santiago creates a hypothesis linked to PAPER-131:

```bash
nk create hypothesis "Fastembed improves entity retrieval latency by >=15% vs graph traversal" \
    --paper 131 --board research --push
# Returns: H131.1 (auto-linked: H131.1 --tests--> PAPER-131)
```

---

**Step 4: Design Experiment**

Santiago designs a reproducible experiment:

```bash
nk create experiment "Fastembed vs Graph Traversal A/B Study" \
    --hypothesis H131.1 --board research --push
# Returns: EXPR-131.1 (auto-linked: EXPR-131.1 --validates--> H131.1)
```

Santiago defines the measure:

```bash
nk create measure "Entity Retrieval Latency" \
    --unit milliseconds --category performance --board research --push
# Returns: M-042
nk update M-042 --related EXPR-131.1
# Now M-042 --measures--> EXPR-131.1
```

---

**Step 5: Pre-Register**

Before running, Santiago locks the experiment design in git:

```bash
git add research/experiments/EXPR-131.1.md
git commit -m "pre-register: EXPR-131.1 locked before execution"
```

The git hash is the pre-registration timestamp — it proves the target was set
before the results were known.

---

**Step 6: Execute**

The experiment requires GPU compute, so Santiago queues it:

```bash
nk training queue EXPR-131.1 \
    --being santiago-bahai \
    --corpus bahai \
    --machine DGX
# Returns: TRAIN-001 queued on DGX
```

DGX claims and runs the job:

```bash
# On DGX:
nk training claim --machine DGX
# Runs the A/B experiment
nk training complete TRAIN-001 --results research/shared/eval-data/expr1311/
```

---

**Step 7: Analyze Results**

Santiago compares actual results to the pre-registered target:

| Measure | Target | Actual | Status |
|---------|--------|--------|--------|
| Entity retrieval latency | >=15% improvement | +18% improvement | **VALIDATED** |

**VALIDATED** — the hypothesis is confirmed. The experiment file's Method and
Results sections are reused directly in the paper.

---

**Step 8: Write Paper**

Santiago documents the validated hypothesis in PAPER-131:

- Method section: from EXPR-131.1 protocol
- Results section: from experiment data
- Analysis: +18% improvement supports the claim

---

**If the result had been different:**

| Measure | Target | Actual | Status |
|---------|--------|--------|--------|
| Entity retrieval latency | >=15% improvement | +8% improvement | **NOT SUPPORTED** |

Santiago would:
1. Examine why the target wasn't met
2. Refine the hypothesis (lower the target, or try a different approach)
3. Design a new experiment (EXPR-131.2) to test the refined hypothesis
4. Document the negative result — refuted hypotheses are valid science

---

## Arrow Schema

Research items use the same Arrow `Item` schema as development items, with
additional fields for research-specific metadata.

### Core Item Schema

```
Fields:
  id:           String      # e.g., "H131.1", "EXPR-131.1", "PAPER-131"
  title:        String      # Human-readable title
  item_type:    String      # paper | hypothesis | experiment | measure | idea | literature
  status:       String      # Research: draft | active | complete | abandoned
                           # Development: backlog | planning | ready | in_progress | review | done
  board:        String      # "research" | "development"
  priority:     String      # low | medium | high
  assignee:     String      # Agent or human assigned
  tags:         List[String]  # e.g., ["perception", "v14.2"]
  related:      List[String]  # Linked item IDs
  depends_on:   List[String]  # Blocker IDs
  body:         String      # Full content (protocol, findings, etc.)
  created_at:   Timestamp
  updated_at:   Timestamp
```

### Relations Schema (RDF Predicates)

```
Fields:
  source_id:   String    # The item making the claim
  predicate:   String    # kb:tests | kb:validates | kb:measures | kb:blocked_by
  target_id:   String    # The item being linked to
  created_at:  Timestamp
```

### Key Predicates

| Predicate | Meaning | Example |
|-----------|---------|---------|
| `kb:tests` | Hypothesis tests a Paper | `H131.1 --tests--> PAPER-131` |
| `kb:validates` | Experiment validates a Hypothesis | `EXPR-131.1 --validates--> H131.1` |
| `kb:measures` | Measure measures an Experiment | `M-042 --measures--> EXPR-131.1` |
| `kb:blocked_by` | Experiment blocked by another item | `EXPR-131.2 --blocked_by--> EXP-872` |

### Querying Research Items

```bash
# List all active hypotheses
nk list --board research --type hypothesis --status active

# List all experiments for a specific paper
nk query "experiments for H131.1"

# Show full chain for a paper
nk hdd registry

# Find orphaned hypotheses (no paper link)
nk hdd validate
```

### Experiment Queue in Arrow

Experiments in the queue are standard Arrow items with additional RDF triples:

```turtle
<#EXPR-131.1> a expr:Experiment ;
    expr:paper <paper:PAPER-131> ;
    expr:hypothesis <hyp:H131.1> ;
    expr:runStatus "queued" ;
    expr:runOn "DGX" ;
    expr:requiresGPU "true"^^xsd:boolean ;
    expr:estimatedMinutes 60 .
```

The `expr:runStatus` is stored as an RDF triple, making the queue queryable via SPARQL:

```bash
# Find all queued experiments
nk query --sparql "SELECT ?label ?status WHERE { ?item a <https://nusy.dev/experiment/Experiment> . ?item <https://nusy.dev/experiment/runStatus> ?status . FILTER(?status = 'queued') }"
```

---

## Quick Reference

### Create Commands

```bash
nk create idea "Research question" --board research --push
nk create literature "Topic survey" --board research --push
nk create paper "Paper Title" --board research --push
nk create hypothesis "Testable claim" --paper 131 --board research --push
nk create experiment "Study title" --hypothesis H131.1 --board research --push
nk create measure "Metric name" --board research --push
```

### Training Queue Commands

```bash
nk training queue EXPR-131.1 --being santiago-bahai --corpus bahai --machine DGX
nk training list --status queued
nk training claim --machine DGX
nk training complete TRAIN-001 --results path/to/results/
nk training fail TRAIN-001 --error "error message"
```

### HDD Diagnostics

```bash
nk hdd registry          # Full paper → hypothesis → experiment → measure chains
nk hdd validate           # Check for orphaned items
nk hdd validate --strict  # Fail CI on warnings
```

### Result Statuses

| Status | Meaning | Action |
|--------|---------|--------|
| **VALIDATED** | Target met or exceeded | Document in paper, merge feature |
| **NOT SUPPORTED** | Target not met | Refine or accept as negative result |
| **PRELIMINARY** | Partial data | Continue collecting |
| **CONFOUNDED** | Design flaw found | Redesign experiment |

**Remember:** NOT SUPPORTED is valid science, not a failure. Paper 118 ("Honest Failure")
documents predictions that confidently failed — and explains why.

---

## References

- HDD Methodology: `docs/HDD-METHODOLOGY.md`
- HDD Quick Start: `docs/HDD-QUICK-START.md`
- HDD Decomposition: `docs/HDD-DECOMPOSITION.md`
- nusy-kanban: [https://github.com/hankh95/nusy-kanban](https://github.com/hankh95/nusy-kanban)
