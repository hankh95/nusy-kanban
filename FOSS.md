# FOSS Extraction Status

**Standalone repo:** https://github.com/hankh95/arrow-kanban
**crates.io:** Not yet published (pre-release — library compiles, binary needs refactoring)

## Extraction Details

- **Source:** `crates/nusy-kanban/` in this monorepo
- **Standalone version:** 0.1.0 (pre-release)
- **License:** MIT (Copyright Hank Head)
- **Tests:** 356 pass standalone
- **Clippy:** Clean

## Dependency Resolution

| Internal Dep | Strategy | Status |
|-------------|----------|--------|
| nusy-arrow-core | Removed (zero imports) | Done |
| nusy-codegraph | Removed (zero imports) | Done |
| nusy-arrow-git | Replaced → `arrow-graph-git` (crates.io) | Done |
| nusy-graph-query | Feature-gated ("embeddings") | Done |
| nusy-graph-review | Feature-gated ("pr", stub) | Pending publish |
| nusy-conductor | Feature-gated ("ci", stub) | Pending publish |

## What's Left

1. **Binary compilation** — `main.rs` needs conditional compilation for feature-gated modules (PR, CI, embeddings, persistence). The library compiles but the CLI binary does not yet.
2. **Publish nusy-graph-query to crates.io** — currently git dependency only
3. **Publish nusy-graph-review** — enables PR workflow feature
4. **CI/CD** — GitHub Actions for test, clippy, fmt, doc, feature matrix
