---
name: work
description: Claim a kanban item, create a branch, and start working
disable-model-invocation: true
allowed-tools: Bash(nk *), Bash(nusy-kanban *), Bash(git *), Bash(cargo *)
---

# Work — Claim and Start

Pick up a kanban item and start working on it.

## Required Argument

`$ARGUMENTS` must be an item ID (e.g., `EX-3001`).

## Steps

### 1. Claim the item

```bash
nk move $ARGUMENTS in_progress --assign "$AGENT_NAME"
```

If `nk` alias is not set, use the full command:
```bash
nusy-kanban --server nats://192.168.8.110:4222 move $ARGUMENTS in_progress --assign "$AGENT_NAME"
```

### 2. Read the item

```bash
nk show $ARGUMENTS
```

Extract phases, acceptance criteria, and constraints.

### 3. Create a feature branch

```bash
git checkout main
git pull origin main
git checkout -b expedition/$ARGUMENTS-short-description
```

### 4. Start implementing

Follow the item's phases in order. Write tests alongside implementation.
Run `cargo test` (Rust) or `pytest` (Python) after each phase.
