# nusy-kanban-server: Setup and Operations Guide

The nusy-kanban-server binary runs on Mini (Mac Mini M4) and serves all kanban
commands via NATS request-reply. All agents (M5, DGX, Mini) connect to it via
`--server nats://192.168.8.110:4222`.

## 1. Architecture Overview

```
┌──────────┐  ┌──────────┐  ┌──────────┐
│   Mini   │  │    M5    │  │   DGX    │
│ (server) │  │ (client) │  │ (client) │
└────┬─────┘  └────┬─────┘  └────┬─────┘
     │    NATS (192.168.8.110:4222)  │
     ├─────────────┼────────────────┤
┌────▼─────────────▼────────────────▼────┐
│    nusy-kanban-server (Mini:4222)      │
│  KanbanStore + ProposalStore + Parquet │
└────────────────────────────────────────┘
```

**Single-writer semantics** — all mutations go through the server. Clients never
write to the Parquet store directly; they send commands over NATS and receive
responses.

**Stores:**

| Store | Purpose |
|-------|---------|
| KanbanStore | Items (expeditions, chores, voyages, etc.) and status transition runs |
| RelationsStore | Relationships between items (related, blocked-by, parent) |
| ProposalStore | Graph-native proposals (PRs) |
| CommentStore | Review comments on proposals |

**Persistence:** Parquet files in `.nusy-kanban/` via nusy-arrow-git's
`save_named_batches()` (WAL + atomic rename). Every mutation is durably written
before the response is sent.

**Implementation:** The server is built on `NatsServiceBuilder` from noesis-ship.
All NATS boilerplate (connect, subscribe, dispatch, shutdown) is handled by the
builder — the server's `run()` function is ~23 lines.

**Events:** Mutations publish to the `KANBAN_EVENTS` JetStream stream (24h
retention, 100k max messages) on `kanban.event.*` subjects. Events are wrapped
in `ShipEvent` envelopes for durability — late-joining agents can replay recent
history, and Command Deck can recover on reconnect.

## 2. Server Setup

### Prerequisites

- Rust toolchain (`rustup` installed, stable channel)
- NATS server running on port 4222 (`nats-server`)

### Install

```bash
cargo install --path crates/nusy-kanban-server
```

### macOS (launchd)

Create the plist file at `~/Library/LaunchAgents/com.nusy.kanban-server.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.nusy.kanban-server</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/hankh19/.cargo/bin/nusy-kanban-server</string>
        <string>--data-dir</string>
        <string>/Users/hankh19/Projects/nusy-product-team</string>
        <string>--nats-url</string>
        <string>nats://localhost:4222</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/nusy-kanban-server.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/nusy-kanban-server.err</string>
</dict>
</plist>
```

Load the service:

```bash
cp com.nusy.kanban-server.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.nusy.kanban-server.plist
```

### Linux (systemd)

Create `/etc/systemd/system/nusy-kanban-server.service`:

```ini
[Unit]
Description=nusy-kanban-server
After=nats-server.service

[Service]
ExecStart=/home/user/.cargo/bin/nusy-kanban-server --data-dir /path/to/repo --nats-url nats://localhost:4222
Restart=always

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl enable nusy-kanban-server
sudo systemctl start nusy-kanban-server
```

## 3. Client Setup

Add the following alias to your shell profile (`~/.zshrc`, `~/.bashrc`, etc.):

```bash
alias nk='nusy-kanban --server nats://192.168.8.110:4222'
```

Reload your shell and verify connectivity:

```bash
nk board
```

All agents (M5, DGX, Mini) use this same alias. The IP `192.168.8.110` is Mini's
LAN address where the NATS server and nusy-kanban-server run.

## 4. NATS Subjects

### Commands (request-reply)

All commands use request-reply on `kanban.cmd.*`:

| Subject | Description |
|---------|-------------|
| `kanban.cmd.create` | Create item |
| `kanban.cmd.move` | Move item status |
| `kanban.cmd.update` | Update item fields |
| `kanban.cmd.comment` | Add comment |
| `kanban.cmd.list` | List with filters |
| `kanban.cmd.board` | Board view |
| `kanban.cmd.show` | Item detail |
| `kanban.cmd.query` | Search |
| `kanban.cmd.stats` | Statistics |
| `kanban.cmd.delete` | Delete item |
| `kanban.cmd.validate` | Board integrity check |
| `kanban.cmd.export` | Export data |
| `kanban.cmd.next-id` | ID allocation |
| `kanban.cmd.history` | Recently completed items |
| `kanban.cmd.hdd.*` | Research board (paper, hypothesis, experiment, measure, idea, literature, validate, registry) |
| `kanban.cmd.relation.*` | Relations (add, query) |
| `kanban.cmd.pr.*` | Proposals (create, list, view, diff, review, merge, close, comment, checks) |

### Events (JetStream durable)

Mutations publish to the `KANBAN_EVENTS` JetStream stream with `ShipEvent`
envelopes. Subscribe for real-time notifications with replay capability:

| Subject | Description |
|---------|-------------|
| `kanban.event.created` | Item created |
| `kanban.event.moved` | Item moved to new status |
| `kanban.event.deleted` | Item deleted |

**Stream config:** `KANBAN_EVENTS`, subjects `kanban.event.>`, 24h retention,
100k max messages, file storage.

**Mutation detection:** The `detect_mutation()` function in
`nusy-kanban-server::events` maps dispatch responses to event types. Only
successful create/move/delete commands emit events.

## 5. Data Layout

```
.nusy-kanban/
  items.parquet        # All kanban items
  runs.parquet         # Status transition history
  relations.parquet    # Item relationships
  proposals.parquet    # Graph-native proposals
  comments.parquet     # Review comments
  _wal.json            # (transient -- only exists during save)
```

All data lives in Arrow RecordBatches in memory and is persisted to Parquet on
every mutation. The `_wal.json` file is part of the WAL (write-ahead log) +
atomic rename strategy provided by nusy-arrow-git's `save_named_batches()`.

## 6. Rebuilding After Code Changes

When `crates/nusy-kanban/`, `crates/nusy-kanban-server/`, or `crates/noesis-ship/`
changes, both the server and client binaries need to be rebuilt. The server
depends on noesis-ship's `NatsServiceBuilder` for its NATS loop.

### Server (Mini only)

```bash
git pull origin main
cargo clean -p nusy-kanban -p nusy-kanban-server
cargo install --path crates/nusy-kanban --force
cargo install --path crates/nusy-kanban-server --force
launchctl kickstart -k gui/$(id -u)/com.nusy.kanban-server

# Smoke test
nk query --search "test" --top 3   # must return exactly 3 items
```

**WARNING:** Skipping `cargo clean -p` in multi-worktree setups causes stale
build cache artifacts — the binary looks rebuilt but runs old code.

### Clients (all machines)

```bash
git pull origin main
cargo install --path crates/nusy-kanban
```

No restart needed on client machines -- each `nk` invocation is a fresh process
that connects to the server.

## 7. Nightly Snapshots

The Parquet store is the live state. For disaster recovery, schedule a nightly
git snapshot of the `.nusy-kanban/` directory.

**macOS (launchd):**

Create `~/Library/LaunchAgents/com.nusy.kanban-snapshot.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.nusy.kanban-snapshot</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>-c</string>
        <string>cd /Users/hankh19/Projects/nusy-product-team &amp;&amp; git add .nusy-kanban/*.parquet &amp;&amp; git commit -m "chore: nightly kanban snapshot" &amp;&amp; git push origin main</string>
    </array>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key>
        <integer>3</integer>
        <key>Minute</key>
        <integer>0</integer>
    </dict>
</dict>
</plist>
```

**Rollback:** If the live store is corrupted, restore from the last good git
snapshot:
```bash
git checkout HEAD~1 -- .nusy-kanban/
launchctl unload ~/Library/LaunchAgents/com.nusy.kanban-server.plist
launchctl load ~/Library/LaunchAgents/com.nusy.kanban-server.plist
```

## 8. Migration from Markdown Files

If you have existing markdown kanban files (from yurtle-kanban or manual
creation), use the `migrate` command to import them into the Arrow store:

```bash
# Dry run — see what would be imported
nk migrate --dry-run

# Import all markdown files from kanban-work/ and research/
nk migrate
```

The migration reads YAML frontmatter from `.md` files, extracts ID, title,
status, priority, assignee, tags, related, and body content, and writes them
into the Arrow store. Original files are left untouched (not deleted).

After migration, verify with `nk board` and `nk stats`, then stop creating
new markdown files — use `nk create` for all new items.

## 9. Troubleshooting

| Problem | Fix |
|---------|-----|
| `server mode failed, falling back to local mode` | NATS server not running or unreachable. Check: `nats-server -v`, verify IP/port, ensure Mini is reachable on the LAN. |
| ID collision | Server allocates IDs starting at 3001+. If you see low IDs, rebuild the server binary. |
| Stale data after merge | Restart server: `launchctl unload` then `launchctl load` the plist. |
| Corrupt Parquet | WAL guarantees atomic writes. If `_wal.json` exists, a previous save was interrupted -- data is safe (atomic rename was not reached). Delete `_wal.json` and restart. |
| Items invisible | Items are in the Arrow store, not files. Use `nk show` / `nk list`, not file browsing. |
| Connection timeout | Check that NATS is listening: `lsof -i :4222`. Verify no firewall rules blocking the port. |
| Server won't start | Check logs at `/tmp/nusy-kanban-server.log` and `/tmp/nusy-kanban-server.err`. |
