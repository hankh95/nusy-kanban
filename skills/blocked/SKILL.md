---
name: blocked
description: Mark an item as blocked with a reason
disable-model-invocation: true
allowed-tools: Bash(nk *), Bash(nusy-kanban *)
---

# Blocked — Mark Item Blocked

Mark a kanban item as blocked and record the reason.

## Required Argument

`$ARGUMENTS` must be an item ID (e.g., `EX-3001`).

## Steps

### 1. Add blocking comment

```bash
nk comment $ARGUMENTS "BLOCKED: <reason>"
```

### 2. Check what blocks it

```bash
nk show $ARGUMENTS
nk blocked
```

### 3. Notify

If the blocker is another item, check its status:

```bash
nk show <blocking-item-id>
```

Report the blocking chain to the user.
