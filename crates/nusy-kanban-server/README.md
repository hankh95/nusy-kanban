# nusy-kanban-server

NATS server + client bridge for the Arrow-native kanban engine.

Provides a persistent NATS request-reply service wrapping the `nusy-kanban` library. All kanban commands are available via `kanban.cmd.*` subjects, and mutations broadcast events to `kanban.event.*` for real-time consumers.

## Features

- **NATS Request-Reply** — all kanban CRUD operations via `kanban.cmd.{command}`
- **Event Broadcasting** — mutations emit events on `kanban.event.*` for Command Deck
- **Startup Backup Check** — snapshots the Arrow store if a backup is due on startup
- **JetStream Durable Events** — event history survives broker restarts
- **Feature-Gated Modules** — PR workflow, research tracking, git source transport

## Quick Start

```bash
# Install server binary
cargo install nusy-kanban-server

# Start server (defaults: nats://localhost:4222, data-dir = current dir)
nusy-kanban-server

# With custom NATS URL and data directory
nusy-kanban-server --nats-url nats://192.168.1.100:4222 --data-dir /path/to/project
```

## Architecture

```
NATS Client → kanban.cmd.{command} → Server → nusy-kanban lib → JSON response
                                              ↓
                              kanban.event.{type} → Command Deck / other consumers
```

## Backup on Startup

The server checks on startup whether a kanban backup is due (based on schedule in `.yurtle-kanban/config.yaml`). If due, it creates a timestamped snapshot of the Arrow store before entering the NATS event loop. This runs in a background thread and does not delay server startup.

## Launchd (macOS)

For scheduled daily backups via launchd, copy and load the plist:

```bash
cp scripts/com.nusy.kanban-backup.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.nusy.kanban-backup.plist
```

## Feature Flags

| Feature | Description |
|---------|-------------|
| `pr` | Graph-native PR workflow (requires nusy-graph-review) |
| `research` | HDD experiment run tracking |
| `git` | Git bundle source transport |

Default features are intentionally empty. Enable `pr` for proposal workflow support:

```toml
nusy-kanban-server = { version = "0.15.0", features = ["pr"] }
```

## License

MIT
