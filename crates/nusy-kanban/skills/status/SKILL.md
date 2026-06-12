---
name: status
description: Show kanban board status and work distribution
disable-model-invocation: false
allowed-tools: Bash(nk *), Bash(nusy-kanban *), Bash(git *)
---

# Status — Board Overview

Show the current state of the kanban board, work distribution, and open proposals.

## Steps

### 1. Board statistics

```bash
nk stats
```

### 2. Current board

```bash
nk board
```

### 3. Work in progress

```bash
nk list --status in_progress
```

### 4. Open proposals

```bash
nk pr list
```

### 5. Blocked items

```bash
nk blocked
```
