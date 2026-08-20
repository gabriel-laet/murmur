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
start`. Murmur never parses or routes (`murmur doctor` reads the kind
column as a lint — never to route); the lead model applies it, the human
curates it, the beads ledger (who closed/reopened what) corrects it. Mixed
herds: `--kind claude,codex=2`.

Multiplexers are views, not competitors: murmur owns durable state; a TUI
should consume `murmur watch --json`, not scrape terminals. `murmur start`
is userland policy — pull a bead, ask herdr for panes, deliver the brief,
then get out of the way. `murmur stop` reads `.murmur/herd.json` and closes
that workspace (never from inside it). Identity is the seam: a herdr pane's agent name
*is* the murmur name, and the herdr plugin nudges idle panes (mail first,
ready beads when the inbox is empty). `--worktree` extends the same policy
to git: one worktree per agent (isolation instead of claims), the lead's
branch as the integration branch, merge discipline in the brief — the
kernel never merges, and PRs stay the git host's job.

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

The take path is assignment-first: the lead assigns a slice by mail and the
worker takes it by id (`task take <id>`); oldest-open-leaf is the fallback
for boards that are genuinely a free-for-all, and unscoped tracker pulls are
guarded (a big `bd ready` set requires `--parent`/`--label`/`--all`). Herds
can be given their own bus (`start --board <name>` → `.murmur-<name>/`), so
concurrent waves on one machine never share agents, mail, or tasks.

There is one assignment, and beads owns it. Beads decides what exists;
murmur decides who has the lock; the board is a projection of `bd ready`,
never a second tracker. Concretely: only leaves pull (a parent of a ready
bead never lands as takeable work, and `task take` refuses a mirrored task
beads no longer offers — even when it is the only file on the board); a
bead closed in beads ends the murmur task no matter who holds the take,
with the ex-holder mailed; a beads assignee beats a conflicting local take
(forced drop, mailed). Mutating `bd` calls ask for `--json` but treat
human-text output on a green exit as success, and closing an
already-closed bead is success — the goal state holds.

The content rule: anything a future session needs (decisions, discovered
work, links) goes to beads; anything only this conversation needs flows
through murmur and is consumed. Three timescales — beads is memory
(days–months), murmur is the nervous system (minutes–hours), herdr is
attention (seconds) — and each layer talks only to its neighbor; herdr never
learns what a bead is. The board slims toward a staging area for beads,
kept because any process that can `mv` participates with no `bd` installed.
Other trackers stay possible as the same adapter shape; beads is the default
because it shares the thesis: state in files, transport you already trust.

## The merge queue and the git host

"Lead owns the integration branch" used to be a sentence in a brief; now it
is `murmur restack`: merge each worker branch into the current checkout one
at a time, gate each merge on a command, warn on declared hub files, stop
with the conflicting files. Merges, never rebases — worker branches are
other people's live checkouts. `murmur pr status` (via `gh`, the same
shell-out shape as `bd` and `herdr`) snapshots each herd branch's PR and
checks, and restack holds a branch whose PR checks are red.

Direction, not yet built: PR events as gates. The natural next step is
"a bead does not close until its PR merges" — `task done` records the PR,
sync closes the bead only when `gh` says merged, and a PR going red mails
the holder. That stays adapter logic (beads owns the gate's truth, the git
host owns CI), and murmur stays a poller over CLIs — no webhooks, no
daemon, in keeping with everything else here.

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

## Cloud agents: a temporary executor

Provider-hosted agents (Cursor cloud, Claude Code on the web) have no
terminal, so herdr can't own them yet. `cloud:<backend>` kinds in `murmur
start` bridge the gap the usual way — userland policy shelling out to a CLI
(`curl`), one backend per match arm, like secrets. The non-goal below
stands: launch is `start`'s policy, and nothing supervises — `cloud
status`/`cloud prompt` are explicit commands, and the run's durable record
is the git host's PR plus the launch id in the lead's mail. A cloud agent
never joins the bus (no inbox, board, or claims — git is its whole
coordination surface), so a cloud kind can't lead a mixed herd. The module
is scaffolding to delete the day herdr grows cloud panes.

## Non-goals

- Spawning, scheduling, or supervising agents (v1's `spawn` was a mistake).
- Prompts, roles, workflows, or any orchestration policy.
- Public-internet endpoints, TLS termination, token formats.
- Storing secret values, ever.
- Exactly-once semantics across machines — deterministic conflict
  resolution instead.
