---
name: done
description: Mark item complete — run tests, commit, push, create proposal
disable-model-invocation: true
allowed-tools: Bash(nk *), Bash(nusy-kanban *), Bash(git *), Bash(cargo *), Bash(pytest *)
---

# Done — Complete and Submit

Finalize work on a kanban item: run tests, commit, push, create proposal.

## Required Argument

`$ARGUMENTS` must be an item ID (e.g., `EX-3001`).

## Steps

### 1. Run tests

```bash
# Rust
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all --check

# Python
pytest -v --tb=short
```

All tests must pass before proceeding.

### 2. Commit

```bash
git add <specific files>
git commit -m "feat($ARGUMENTS): Brief description

Co-Authored-By: Claude <noreply@anthropic.com>"
```

### 3. Push and create proposal

```bash
git push -u origin HEAD
nk pr create --title "$ARGUMENTS: Title" --base main
```

### 4. Move to review

```bash
nk move $ARGUMENTS review
```
