# FOSS Extraction Status

**Standalone repo:** https://github.com/hankh95/arrow-core
**crates.io:** https://crates.io/crates/arrow-graph-core (published v0.1.0)

## Extraction Details

- **Source:** `crates/nusy-arrow-core/` in this monorepo
- **Standalone version:** 0.1.0
- **License:** MIT
- **Internal deps:** None (fully self-contained — only arrow, parquet, uuid, chrono, thiserror)

## What Changed in Standalone

1. **Namespace generalized** — hardcoded 5-variant `Namespace` enum replaced with string-based partition keys (`ArrowGraphStore::new(&["world", "code", "self"])`)
2. **YLayer generalized** — hardcoded 7-layer `YLayer` enum replaced with optional `u8` layer column (column renamed `y_layer` → `layer`)
3. **cognitive_params.rs excluded** — NuSy V15 self-evolution specific, not generic
4. **KgStore prefixes** — NuSy-specific prefixes removed (santiago, ethics, pm, dev, nusy); standard RDF prefixes retained (rdf, rdfs, owl, xsd, foaf, prov)
5. **graph_factory simplified** — hardware detection removed; config uses string namespaces
6. Added: `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `.github/dependabot.yml`
7. Added: `README.md`, `LICENSE` (MIT), `.gitignore`

## Reconnection Plan

This monorepo continues using the internal `path = "../nusy-arrow-core"` dependency
with the NuSy-specific `Namespace` enum and `YLayer`. Once the standalone crate is
published on crates.io, downstream consumers can use:

```toml
arrow-graph-core = "0.1"
```

The monorepo path dep remains until EX-3214 migrates to the published version.
