---
name: handoff
description: Session handoff — summarize work done and what's next
disable-model-invocation: true
allowed-tools: Bash(nk *), Bash(nusy-kanban *), Bash(git *)
---

# Handoff — Session Summary

Summarize what was accomplished in this session and what's next.

## Steps

### 1. Review recent commits

```bash
git log --oneline -10
```

### 2. Check items touched

```bash
nk list --status in_progress
nk list --status review
```

### 3. Generate handoff summary

Report:
- Items completed (moved to done)
- Items in progress (current branch, what's left)
- Items in review (awaiting cross-agent review)
- Blockers discovered
- Suggested next steps

### 4. Push any uncommitted work

```bash
git status
# If changes exist:
git add <files>
git commit -m "wip: session handoff"
git push origin HEAD
```
