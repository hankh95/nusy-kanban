# Contributing to nusy-graph-query

Thank you for your interest in contributing!

## Development

This crate is part of the [nusy-product-team](https://github.com/hankh95/nusy-product-team)
workspace. To work on it:

```bash
git clone https://github.com/hankh95/nusy-product-team
cd nusy-product-team

# Build just this crate
cargo build -p nusy-graph-query

# Run tests
cargo test -p nusy-graph-query

# Run with feature flags
cargo test -p nusy-graph-query --features ollama
```

## Code Standards

- **No `.unwrap()` in library code** — use `?` or `let-else` with graceful fallbacks
- **Named column constants** — use `EdgeSchema` instead of magic column indices
- **Arrow-native** — operate on `RecordBatch` columns directly, no intermediate materialization
- `cargo clippy -- -D warnings` must pass
- `cargo fmt` must pass

## Pull Requests

All PRs go through graph-native review via `nk pr` (not GitHub PRs).
See the project's `CLAUDE.md` for the full workflow.

## License

By contributing, you agree that your contributions will be licensed under MIT.
