# murmur

The communication kernel for AI agents. A directory of files, not a daemon.

Murmur doesn't orchestrate agents and doesn't secure networks — it gives agents
mechanism (messaging, presence, claims, a task board, secret references) built
on the trust you already have: your filesystem, your ssh keys, your private
network, your secret manager. Design and remote/p2p roadmap: [DESIGN.md](DESIGN.md).

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
murmur setup    # wires hooks + MCP into the current repo, idempotently
```

## Quick start

```bash
# Announce yourself (optional — sending or reading mail also registers you)
murmur join backend

# Message a peer. They don't need to be running — it waits in their inbox.
murmur send frontend "API is ready at /v2/users" --as backend

# Broadcast to everyone
murmur send '*' "rebasing main, hold your pushes" --as backend

# Ask a question and block until the answer comes back
murmur send db-agent "is the schema final?" --as backend --reply

# Read your mail (consumes it); --wait blocks until something arrives
murmur inbox --as frontend
murmur inbox --as frontend --wait --timeout 120

# Who's around?
murmur who

# Watch all agent chatter live (for the human running the show)
murmur watch
```

## The task board

A shared work queue with no queue server. A task is a file; its state is which
directory it's in. **Taking a task is an atomic rename** — when five agents race
for the same task, exactly one rename succeeds and the losers grab the next
file. Work-stealing semantics, zero infrastructure:

```bash
murmur task add "write auth integration tests" --body "cover the refresh flow" --as lead
murmur task add "update API docs" --as lead

murmur task take --as worker-1     # atomically yours (oldest first)
murmur task done <id> --as worker-1
murmur task drop <id> --as worker-1   # couldn't finish? back on the board
murmur task list                      # todo + doing (--all includes done)
```

Point N agents at the same board and they self-organize: each takes a task,
works, completes, takes the next. The hook tells idle agents when the board
has open work.

### Linear adapter

Humans plan in Linear; agents execute on the board; status flows back:

```bash
export LINEAR_API_KEY=lin_api_...
murmur task sync linear --team ENG --label agents
```

Sync is bidirectional and idempotent — run it by hand, from a hook, or on a
cron. Open issues (optionally filtered by label) land on the board as
`linear-ENG-42`-style tasks, so `murmur task done linear-ENG-42` reads
naturally and re-syncs never duplicate. Local transitions push back as
workflow-state changes with an attributed comment: take → *In Progress*
("Taken by agent `worker-1` via murmur"), done → *Done*, drop → back to
*Todo*. Linear keeps planning, priorities, and history; the board keeps the
atomic-take mechanics. Transport is `curl` against Linear's GraphQL API — no
SDK, and the key never touches disk.

## Request/reply

Messages are one-way by default, but `--reply` turns one into a blocking
question — the sender waits while the recipient answers at its own pace:

```bash
# agent A blocks here...
murmur send b "did you migrate the users table?" --as a --reply --timeout 120

# agent B sees the question (inbox/hook shows the reply command ready to paste):
#   [10:04:11] a: did you migrate the users table?
#     ↳ sender is waiting: murmur send a "..." --reply-to <id>
murmur send a "yes, plus indexes" --as b --reply-to <id>

# ...and A's blocked command prints: yes, plus indexes
```

The reply is correlated by message id, so normal traffic keeps flowing around
it. If nobody answers in time, the question stays in their inbox — it degrades
back into a plain durable message.

Identity comes from `--as <name>` or the `MURMUR_AGENT` environment variable.
State lives in `.murmur/`, discovered like `.git` by walking up from the
current directory (override with `MURMUR_DIR`). It ignores itself in git.

## Remote: sync, not servers

Murmur crosses machines by reconciling `.murmur/` directories, not by opening
ports. Each node appends to its own log; a sync exchanges what the other side
is missing, merges state, and rebuilds inboxes. No daemon anywhere, nothing
whose death loses data:

```bash
# over ssh — auth, encryption, and revocation are your existing ssh keys
murmur sync dev@buildbox:work/myrepo

# or any path two machines can both see (shared volume, sshfs, second checkout)
murmur sync /mnt/shared/myrepo
```

Sync is idempotent and relays: A↔B then B↔C gives C everything from A, because
logs are per-origin-node. Consumed messages leave tombstones, so mail read on
one machine disappears everywhere and never resurrects. `murmur who` shows
remote agents with their node; broadcasts reach them once presence has synced;
`murmur watch` shows cluster-wide chatter. Run a sync by hand, from a hook, or
on a cron — at agent timescales, once per turn is live enough.

Honest physics: there is no atomic rename across machines. If two nodes take
the same task during a partition, the conflict resolves deterministically on
sync (lexicographically smaller holder keeps it, on both sides) and the
loser's `task done` fails visibly. Trust is git-pull-style: syncing with a
host means trusting it — pick your peers the way you pick your git remotes.

## Secrets: references, never values

The bus is plaintext files, so secret *values* never touch it. Secrets are
first-class as **references** — ordinary text that grants nothing by itself:

```bash
# alice shares where a secret lives (Infisical-native), not the secret
murmur send bob "DB creds: secret://infisical/proj-123/dev/DATABASE_URL" --as alice

# bob resolves at his edge, with HIS OWN Infisical identity (INFISICAL_TOKEN etc).
# The value goes straight into the child env — never stdout, never agent context:
murmur secret exec DATABASE_URL=secret://infisical/proj-123/dev/DATABASE_URL -- psql
```

If bob's identity can't read that secret, resolution fails at his edge by
Infisical's policy — that's the system working. Access control, rotation, and
audit stay in the secret manager; murmur shells out to the `infisical` CLI and
never stores a secret byte. Hooks and MCP never auto-resolve: a ref arriving
in an agent's context is labeled "never resolve this into your context."
A `secret://env/<VAR>` backend covers the trivial local case; more backends
are one match arm each. See [DESIGN.md](DESIGN.md) for the invariants.

## Claude Code integration

`murmur setup` writes all of the below for you (merging with existing config,
never clobbering). The details, if you want them by hand:

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

- **SessionStart** — the agent joins, learns its name, who else is here, and
  whether the task board has open work.
- **PreToolUse** — pending messages are injected into the agent's context
  (questions arrive with the exact reply command). Editing a file claimed by
  another agent is denied with an explanation of who holds it and how to
  coordinate. Free files are claimed automatically.
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

Exposes `send_message`, `broadcast`, `ask` (blocking request/reply),
`check_inbox`, `list_agents`, `claim_file`, `release_file`, `add_task`,
`take_task`, `complete_task`, and `list_tasks`.

### No integration at all

Agents can also just run the CLI from their shell tool — or skip murmur
entirely and read the files. The format *is* the protocol:

```text
.murmur/
  agents/<name>.json     presence: pid, cwd, joined_at, last_seen
  inbox/<name>/*.json    one file per pending message, oldest-first by name
  claims/*.json          advisory file claims with TTLs
  tasks/todo/*.json      open tasks — take one by renaming it into doing/
  tasks/doing/*.json     in-progress tasks (taken_by inside)
  tasks/done/*.json      finished tasks
  log.jsonl              append-only record of every message
  tmp/                   staging for atomic renames
```

A message is `{"id", "from", "to", "ts", "body"}` plus optional `re` (the id
it replies to) and `wants_reply`. To send one by hand: write the JSON to
`tmp/`, then `mv` it into `inbox/<recipient>/`. The rename is atomic, so
readers never see a partial message. `body` is an opaque string — plain text,
JSON, whatever you and your peers agree on.

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
                           #   --reply blocks for an answer; --reply-to <id> answers
murmur inbox               # read + consume your mail (--wait, --peek, --json)
murmur who                 # list agents and liveness
murmur join [name]         # register presence, see peers
murmur leave               # deregister, release claims
murmur claim <path>        # advisory claim (--ttl secs)
murmur release <path>      # release a claim
murmur claims              # list active claims
murmur task add|list|take|done|drop   # shared work queue
murmur task sync linear --team ENG    # reconcile the board with Linear
murmur secret exec NAME=<ref> -- cmd  # resolve secret refs into a command's env
murmur secret resolve <ref>           # resolve to stdout (prefer exec)
murmur log [-n N]          # recent message history
murmur watch               # follow all traffic live (--all for history)
murmur clean               # prune dead agents + expired claims (--all: rm .murmur)
murmur sync <peer>         # reconcile with another .murmur (path or user@host[:path])
murmur setup               # wire hooks + MCP into this repo
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
