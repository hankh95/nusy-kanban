---
name: review
description: Self-review checklist before submitting for cross-agent review
disable-model-invocation: true
allowed-tools: Bash(nk *), Bash(nusy-kanban *), Bash(git *), Bash(cargo *)
---

# Review — Self-Review Checklist

Run through the self-review checklist before submitting for cross-agent review.

## Required Argument

`$ARGUMENTS` — optional item ID for context.

## Steps

### 1. Review the full diff

```bash
git diff main...HEAD --stat
git diff main...HEAD
```

### 2. Phase completeness

Load the item and verify ALL phases are addressed:

```bash
nk show $ARGUMENTS
```

For each phase, confirm the PR contains work addressing it.

### 3. Architecture checklist

| Question | Answer |
|----------|--------|
| No duplication? | |
| Tests cover all changes? | |
| No over-engineering? | |

### 4. Quality checks

```bash
# Rust
cargo clippy --workspace -- -D warnings
cargo fmt --all --check

# Python
mypy brain/
```

### 5. Fix all issues found

Do not leave issues for the reviewer. Fix them now and re-run tests.
