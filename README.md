# murmur

Local message passing for AI agents. A directory of files, not a daemon.

Agents are intermittent — they exist for a few seconds between tool calls, then
they're gone until the next turn. Sockets, brokers, and HTTP all assume someone
is listening *right now*. The filesystem doesn't. So in murmur, a message is a
file atomically renamed into the recipient's inbox, where it waits until they
read it. Presence is a file. File claims are files. The whole system is a
`.murmur/` directory you can inspect with `ls` and `cat`, and any agent that
can read and write files can participate — even without murmur installed.

No daemon. No sockets. No ports. No auth. Nothing to keep alive.

## Install

```bash
cargo install --path .
```

## Quick start

```bash
# Announce yourself (optional — sending or reading mail also registers you)
murmur join backend

# Message a peer. They don't need to be running — it waits in their inbox.
murmur send frontend "API is ready at /v2/users" --as backend

# Broadcast to everyone
murmur send '*' "rebasing main, hold your pushes" --as backend

# Read your mail (consumes it); --wait blocks until something arrives
murmur inbox --as frontend
murmur inbox --as frontend --wait --timeout 120

# Who's around?
murmur who

# Watch all agent chatter live (for the human running the show)
murmur watch
```

Identity comes from `--as <name>` or the `MURMUR_AGENT` environment variable.
State lives in `.murmur/`, discovered like `.git` by walking up from the
current directory (override with `MURMUR_DIR`). It ignores itself in git.

## Claude Code integration

### Hooks (zero-config coordination)

Add to `.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [{"hooks": [{"type": "command", "command": "murmur hook"}]}],
    "PreToolUse":   [{"matcher": "", "hooks": [{"type": "command", "command": "murmur hook"}]}],
    "PostToolUse":  [{"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "murmur hook"}]}],
    "Stop":         [{"hooks": [{"type": "command", "command": "murmur hook"}]}],
    "SessionEnd":   [{"hooks": [{"type": "command", "command": "murmur hook"}]}]
  }
}
```

Then every Claude Code session in the workspace coordinates automatically:

- **SessionStart** — the agent joins and is told its name and who else is here.
- **PreToolUse** — pending messages are injected into the agent's context.
  Editing a file claimed by another agent is denied with an explanation of who
  holds it and how to coordinate. Free files are claimed automatically.
- **PostToolUse** — claims are released after the edit.
- **Stop** — an agent can't end its turn with unread mail; messages that
  arrived mid-task are delivered as the block reason so it can act on them.
- **SessionEnd** — the agent leaves and its claims are released.

Set `MURMUR_AGENT=frontend` in the session's environment to pick your own
name; otherwise one is derived from the session id.

### MCP (proactive messaging)

For agents that should *send* messages, not just receive them:

```json
{
  "mcpServers": {
    "murmur": {
      "command": "murmur",
      "args": ["mcp"],
      "env": { "MURMUR_AGENT": "backend" }
    }
  }
}
```

Exposes `send_message`, `broadcast`, `check_inbox`, `list_agents`,
`claim_file`, and `release_file`.

### No integration at all

Agents can also just run the CLI from their shell tool — or skip murmur
entirely and read the files. The format *is* the protocol:

```text
.murmur/
  agents/<name>.json     presence: pid, cwd, joined_at, last_seen
  inbox/<name>/*.json    one file per pending message, oldest-first by name
  claims/*.json          advisory file claims with TTLs
  log.jsonl              append-only record of every message
  tmp/                   staging for atomic renames
```

A message is `{"id", "from", "to", "ts", "body"}`. To send one by hand: write
the JSON to `tmp/`, then `mv` it into `inbox/<recipient>/`. The rename is
atomic, so readers never see a partial message. `body` is an opaque string —
plain text, JSON, whatever you and your peers agree on.

## File claims

Claims are advisory locks that stop parallel agents from stomping on each
other's edits. They expire on their own (default 15 minutes), so a crashed
agent can never deadlock the team.

```bash
murmur claim src/auth.rs --as backend       # yours for 15 min
murmur claim src/auth.rs --as frontend      # error: claimed by backend
murmur claims                                # list active claims
murmur release src/auth.rs --as backend
```

With the hook installed this is automatic around every Edit/Write.

## All commands

```bash
murmur send <to> [msg]     # deliver a message ('*' = broadcast; stdin if no msg)
murmur inbox               # read + consume your mail (--wait, --peek, --json)
murmur who                 # list agents and liveness
murmur join [name]         # register presence, see peers
murmur leave               # deregister, release claims
murmur claim <path>        # advisory claim (--ttl secs)
murmur release <path>      # release a claim
murmur claims              # list active claims
murmur log [-n N]          # recent message history
murmur watch               # follow all traffic live (--all for history)
murmur clean               # prune dead agents + expired claims (--all: rm .murmur)
murmur mcp                 # MCP server over stdio
murmur hook                # Claude Code hook adapter
```

## Design notes

- **Durable by default.** Sending to an agent that hasn't started yet works;
  the message waits. This is the property agents actually need, and it's the
  one thing a socket can't give you.
- **Crash-safe.** All state is files; there is no process whose death loses
  messages, locks, or the registry.
- **Observable.** `murmur watch` (or `tail -f .murmur/log.jsonl`) shows every
  message. Multi-agent systems fail silently without this.
- **Polling, not push.** `--wait` and `watch` poll at 100–200 ms, which is
  invisible at agent timescales and keeps the implementation dependency-free.
- **Single machine by design** — though a shared volume between containers
  works for free, which sockets never could.
- **Liveness** is "is the pid that invoked murmur still alive", tracked via
  the parent process, plus a `last_seen` timestamp. Good enough to answer
  "is anyone actually there?" without heartbeats.

## License

MIT
