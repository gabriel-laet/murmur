# murmur — design

Murmur is a **communication kernel** for AI agents, not an orchestrator. It
gives agents mechanism — messaging, presence, advisory claims, a shared task
board, secret references — and leaves policy (who spawns whom, what work
happens) to userland: harnesses, humans, scripts. Like a kernel, it should
be small enough to trust by reading it.

## Thesis: the filesystem is the transport

Agents are intermittent — alive for seconds between tool calls. Any
transport needing a live listener (sockets, HTTP) forces a broker, and the
broker becomes a server that owns state. (v1 walked that road: sockets grew
retries, failover, a mesh, then an orchestrator daemon that lost everything
on restart.) The transport every agent already has is the filesystem:

- A message is a file: write to `tmp/`, atomic rename into `inbox/<agent>/`.
  Durable by default — send to an agent that isn't running yet; it waits.
- Presence is a file; liveness is "is that pid alive."
- A claim is a file with a TTL — a crashed agent can't deadlock anyone.
- A task's state is its directory; taking it is `rename(todo/X, doing/X)`,
  so N racing agents get exactly one winner.
- Observability is `tail -f log.jsonl`.

Any process that can `ls`, `cat`, `mv` participates without murmur
installed. The CLI, MCP server, and hooks are thin adapters over one
directory.

## Cross-harness: the adapter surface is the product

The gap that matters is between *tools*, not between two sessions of one
tool. Vendor messaging is single-vendor forever; multiplexer "messaging" is
scraped terminal state that dies with the process. The filesystem is the one
transport Claude, Codex, Gemini, Grok, and a shell script genuinely share.

Three integration tiers, one directory: **hooks** (passive, enforced —
Claude Code today), **MCP** (proactive tools, wired into each harness's own
config by `murmur setup`, with `MURMUR_HARNESS` stamping heterogeneous
default names), and **AGENTS.md** (the written contract, including the raw
file protocol for agents with no murmur at all). Adding a harness must stay
a ~50-line adapter, never a fork of semantics.

Heterogeneity is *why* coordination is worth anything — same-model agents
reduce coordination to parallelism. Which kind fits which work is judgment,
so it is data, not code: `FLEET.md` (seeded by setup, rides git) is a
human-curated roster pasted verbatim into the lead's brief at `murmur
start`. Murmur never parses or routes; the lead model applies it, the human
curates it, the beads ledger (who closed/reopened what) corrects it. Mixed
herds: `--kind claude,codex=2`.

Multiplexers are views, not competitors: murmur owns durable state; a TUI
should consume `murmur watch --json`, not scrape terminals. `murmur start`
is userland policy — pull a bead, ask herdr for panes, deliver the brief,
then get out of the way. Identity is the seam: a herdr pane's agent name
*is* the murmur name, and the herdr plugin nudges idle panes (mail first,
ready beads when the inbox is empty).

## Security by delegation

Murmur never owns crypto, identity, or network policy when a layer that
already owns them exists:

| Boundary             | Owner                                   | Murmur's part                    |
| -------------------- | --------------------------------------- | -------------------------------- |
| Local access         | Unix file permissions on `.murmur/`     | nothing — document it            |
| Remote, phase 1      | ssh (keys, encryption, revocation)      | speak over ssh's pipe            |
| Remote, phase 2      | private networks (Tailscale, WireGuard) | bind private interfaces **only** |
| Mesh membership      | tailnet ACLs                            | thin node allowlist on top       |
| Secret values        | the secret manager (Infisical, …)       | transport references, not values |
| Task planning/memory | beads (`bd`, rides git)                 | sync adapter; board stays kernel |

Two things do belong in the kernel: **provenance** (`from` is self-asserted
today — fine locally, not across nodes; per-node logs get signed entries
with phase 2's keypairs) and **injection framing** (messages are untrusted
input to the receiving model; hook-injected mail is labeled with its origin,
secret refs arrive with do-not-resolve instructions, and the append-only log
is the audit trail). Murmur refuses to own per-agent authorization (the
OS's job), sandboxing (the harness's), encryption at rest (the disk's), and
roles/permissions (userland).

## Secrets: references, never values

The bus is plaintext files, so values never touch it:

```
secret://infisical/<projectId>/<env>/[folders/]<NAME>
secret://env/<VAR>
```

Possessing a ref grants nothing — resolution happens at the receiving edge
through the receiver's *own* backend identity; access control, rotation, and
audit stay in the secret manager. Invariants: hooks and MCP never
auto-resolve; the consumption path is `secret exec` (value goes only into
the child env, never stdout or a context window); a leaked `.murmur/` leaks
an audit trail, not credentials; a ref the receiver can't read fails at
*their* edge, by the backend's policy. Backends are one match arm shelling
out to a known CLI — refs never execute arbitrary commands, because message
bodies are attacker-controlled.

## Beads: the board's upstream

Beads (`bd`) — local-first, git-distributed, dependency-aware — owns
planning, priorities, and memory across sessions. The board owns the agent
mechanics (atomic take, holder-checked done), because N racing agents need
filesystem atomicity. `task sync beads` reconciles both ways, idempotently:
only *ready* beads pull onto the board (never offer blocked work), keeping
their ids; transitions push back (take → in_progress + assignee, done →
closed with attribution, drop → open); `synced_state` makes pushes
non-repeating.

The content rule: anything a future session needs (decisions, discovered
work, links) goes to beads; anything only this conversation needs flows
through murmur and is consumed. Three timescales — beads is memory
(days–months), murmur is the nervous system (minutes–hours), herdr is
attention (seconds) — and each layer talks only to its neighbor; herdr never
learns what a bead is. The board slims toward a staging area for beads,
kept because any process that can `mv` participates with no `bd` installed.
Other trackers stay possible as the same adapter shape; beads is the default
because it shares the thesis: state in files, transport you already trust.

## Remote: replication, not networking

Remote murmur is a replication problem: how do two `.murmur/` directories
reconcile? The data model was made replication-friendly: node identity
(ids are `ts-node-seq`, addressing grows `agent@node`), per-node logs
(`log/<node>.jsonl` — appends never conflict; the line count is the sync
vector), tombstones (consumed mail never resurrects), and a deterministic
task-conflict rule (no cross-machine atomic rename exists; under partition
the lexicographically smaller holder keeps a doubly-taken task, the loser's
`task done` fails visibly — occasional duplicated work is the accepted
price).

Transport phases:

- **Phase 0 (built)**: any shared filesystem, or `murmur sync <path>`.
- **Phase 1 (built)**: `murmur sync <host>` — one-shot anti-entropy over
  ssh (`ssh host murmur sync --stdio`): exchange seen-vectors, stream
  missing entries, merge, rebuild inboxes. No daemon, no ports, no new auth;
  trust is git-pull-style. Relay works because logs are per-origin.
- **Phase 1.5 (built)**: `.murmur/peers` + opportunistic auto-sync — forced
  after `send`, debounced (30s default) on `inbox`, in the hook, and in the
  idle-wake. Best-effort, non-interactive ssh (BatchMode, 3s timeout): a
  dead peer warns, never hangs. One-way reachability suffices; every sync
  exchanges both directions.
- **Phase 2 (deliberately deferred)**: `murmur peer`, a per-machine
  replicator daemon (QUIC, LAN discovery) for push latency, private
  interfaces only. The rule that keeps it from becoming v1's orchestrator:
  servers own state; peers own copies. Kill the daemon and nothing is lost.

## Non-goals

- Spawning, scheduling, or supervising agents (v1's `spawn` was a mistake).
- Prompts, roles, workflows, or any orchestration policy.
- Public-internet endpoints, TLS termination, token formats.
- Storing secret values, ever.
- Exactly-once semantics across machines — deterministic conflict
  resolution instead.
