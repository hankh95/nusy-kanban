---
name: sync
description: Pull latest changes and show board status
disable-model-invocation: false
allowed-tools: Bash(nk *), Bash(nusy-kanban *), Bash(git *)
---

# Sync — Pull and Show Status

Pull the latest changes from main and display the current board status.

## Steps

### 1. Pull latest

```bash
git fetch origin main
git log --oneline origin/main -5
```

### 2. Board overview

```bash
nk board
nk stats
```

### 3. Items in progress

```bash
nk list --status in_progress
```

### 4. Items in review

```bash
nk list --status review
nk pr list
```
