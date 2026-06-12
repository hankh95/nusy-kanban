# FOSS Extraction Status

**Standalone repo:** https://github.com/hankh95/nusy-graph-query
**crates.io:** https://crates.io/crates/nusy-graph-query (pending publish)

## Extraction Details

- **Source:** `crates/nusy-graph-query/` in this monorepo
- **Standalone version:** 0.1.0
- **License:** MIT
- **Internal deps:** None (fully self-contained)

## What Changed in Standalone

1. `Cargo.toml` — workspace references replaced with pinned versions
2. `src/lib.rs` — removed NuSy-specific references from crate-level docs
3. `src/traversal.rs` — fixed doc comment HTML tag (`List<Utf8>` → `` `List<Utf8>` ``)
4. Added: `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `.github/dependabot.yml`
5. Added: `README.md`, `LICENSE` (MIT), `.gitignore`

## Reconnection Plan

This monorepo continues using the internal `path = "../nusy-graph-query"` dependency.
Once the standalone crate is published on crates.io, downstream consumers can use:

```toml
nusy-graph-query = "0.1"
```

The monorepo path dep remains until a future expedition migrates to the published version.
