# murmur — design

Murmur is a **communication kernel** for AI agents. It is not an orchestrator.
It gives agents mechanism — messaging, presence, advisory claims, a shared
task board, secret references — and leaves policy (who spawns whom, what work
happens, what agents may do) to userland: harnesses, humans, scripts.

Like a kernel, it should be small enough to trust by reading it.

## Thesis: the transport agents already share

Agents are intermittent processes. They exist for a few seconds between tool
calls, then vanish until their next turn. Any transport that requires a live
listener at the moment of send (sockets, HTTP) forces you to build a broker —
and the broker becomes a server that owns state, which is exactly the
infrastructure murmur exists to avoid. (v1 of this project walked that road:
sockets grew retries, then host failover, then a mesh, then an orchestrator
daemon with in-memory mailboxes that lost everything on restart.)

The transport every agent in every harness already has is the **filesystem**:

- A message is a file. Delivery is write-to-`tmp/` + atomic rename into
  `inbox/<agent>/`. Readers never see a partial message.
- The mailbox is durable by default. Send to an agent that is busy, asleep,
  or not yet started — the message waits.
- Presence is a file (name, pid, cwd). Liveness is "is that pid alive."
- A claim is a file with a TTL, so a crashed agent cannot deadlock anyone.
- A task is a file; its state is which directory it is in. Taking a task is
  `rename(todo/X, doing/X)` — atomic, so N racing agents get exactly one
  winner. Work-stealing with zero infrastructure.
- Observability is `tail -f log.jsonl`. Humans watch their agents talk.

Any process that can `ls`, `cat`, and `mv` can participate without murmur
installed. The CLI, the MCP server, and the Claude Code hook are thin
adapters over the same directory — the protocol is something agents already
speak.

## Cross-harness: the adapter surface is the product

The coordination gap people actually feel in 2026 is not between two
sessions of one tool — it is between *tools*. Vendor messaging (Claude
Code's cross-session feature) is polished but constitutionally single-vendor:
it will never deliver a message from Codex to Grok. Terminal multiplexers
(herdr, Claude Squad, …) coordinate whatever runs in their panes, but their
"messaging" is scraped terminal state owned by a live process — kill the
multiplexer and the coordination is gone, and nothing crosses a machine
boundary. The filesystem thesis is the answer to exactly this: the one
transport Claude, Codex, Gemini, Grok, and a shell script genuinely share.

So the kernel stays small and the *adapters* carry the strategy. Three
integration tiers, all over the same directory:

1. **Hooks** — passive, enforced coordination (inbox injection, claim
   denial). Claude Code today; any harness that grows a hook surface gets
   the same adapter.
2. **MCP** — proactive tools, one stdio server, wired by `murmur setup`
   into each harness's own config format (`.mcp.json`,
   `~/.codex/config.toml`, `.gemini/settings.json`, `.grok/settings.json`,
   `opencode.json`). Setup stamps `MURMUR_HARNESS` into each config so
   default agent names expose the fleet's heterogeneity (`claude-…`,
   `codex-…`) in `murmur who`.
3. **AGENTS.md** — the written contract for everything else, including
   agents with no murmur binary at all (the raw file protocol is in the
   contract). This is "the format is the protocol", one level up.

Adding a harness must stay a ~50-line adapter, never a fork of semantics.

Heterogeneity is also *why* coordination is worth anything: if every agent
were the same model, coordination would collapse into parallelism. The
knowledge of which kind fits which work is judgment, so it is data, not
code: `FLEET.md` (next to `AGENTS.md`, seeded by setup, rides git) is a
human-curated roster that `murmur start` pastes verbatim into the lead's
brief. Murmur never parses it and never routes — the lead model applies it,
the human curates it, and the beads ledger (which kind closed and reopened
what) is the feedback loop that corrects it. A mixed herd is
`--kind claude,codex=2`; the kernel's only contribution is spawning the mix
and telling every agent what its peers are.

Multiplexers are not competitors; they are **views**. Murmur owns durable
shared state; a control-room TUI that wants to render agent chatter should
consume `murmur watch --json` (stable, newline-delimited) rather than scrape
terminals. Being the state layer under other people's dashboards is the
position; competing on panes is not.

`murmur start` is userland policy in the same slot as `task sync beads`:
it pulls a bead onto the board and, if herdr is running, asks herdr to
open panes and prompt a named herd. It does not live in the kernel, does
not supervise those processes, and does not replace herdr's `agent prompt`
with a mailbox. After the brief is delivered, coordination is murmur
again — tasks, inboxes, claims. Identity is the seam: inside a herdr pane,
the pane's agent name *is* the murmur name; a herdr plugin (`murmur herdr`)
nudges idle panes when mail is waiting.

## Security by delegation

A kernel does not invent trust; it enforces boundaries that already exist.
Murmur never owns crypto, identity, or network policy when a layer that
already owns them is available:

| Boundary            | Owner                                    | Murmur's part                    |
| ------------------- | ---------------------------------------- | -------------------------------- |
| Local access        | Unix file permissions on `.murmur/`      | nothing — document it            |
| Remote, phase 1     | ssh (keys, encryption, revocation)       | speak over ssh's pipe            |
| Remote, phase 2     | private networks (Tailscale, WireGuard)  | bind private interfaces **only** |
| Mesh membership     | tailnet ACLs                             | thin node allowlist on top       |
| Secret values       | the secret manager (Infisical, …)        | transport references, not values |
| Task planning/memory | beads (`bd`, rides git)                 | sync adapter; board stays kernel |

Two things do belong in the kernel:

1. **Provenance.** `from` is self-asserted today — acceptable locally (one
   trust domain), unacceptable across nodes. With per-node logs, each entry
   gets signed by its node key: verified origin, tamper-evident history.
2. **Injection framing.** A message from another agent is untrusted input to
   the receiving model. Murmur cannot fix prompt injection (that is the
   harness's and model's job) but it refuses to make it worse: hook-injected
   mail is labeled with its (eventually verified) origin, and secret
   references arrive with explicit do-not-resolve instructions. The
   append-only log is the audit trail; `murmur watch` is intrusion detection
   you can read.

Murmur explicitly refuses to own: per-agent authorization within a machine
(the OS's job), sandboxing what agents do with messages (the harness's job),
encrypting the log at rest (disk encryption's job), and agent roles or
permissions (userland policy).

## Secrets: references, never values

The bus is plaintext files on disk — so secret *values* must never touch it.
Instead, secrets are first-class as **references**:

```
secret://infisical/<projectId>/<env>/[folders/]<NAME>
secret://env/<VAR>
```

A ref is ordinary text. It travels through inboxes, the log, broadcasts, and
(eventually) remote sync safely, because **possessing a ref grants nothing**:
resolution happens at the receiving edge through the receiver's *own* backend
identity (e.g. `INFISICAL_TOKEN`, machine identity). Access control,
encryption, rotation, and access audit stay in the secret manager, which
already does them well. Murmur shells out to the backend's CLI — it takes no
SDK dependency and never stores a secret byte.

Invariants:

- Hooks and the MCP server **never auto-resolve** a ref. A ref appearing in
  an agent's context arrives labeled "never resolve this into your context."
- The recommended consumption path is `murmur secret exec NAME=<ref> -- <cmd>`,
  which resolves into the child process environment — the value never touches
  stdout, the log, or a model's context window. `murmur secret resolve`
  exists for plumbing but warns.
- A leaked `.murmur/` directory leaks who shared which ref with whom — an
  audit trail, not a credential.
- Sender and receiver need not have the same access: sharing a ref to a
  secret the receiver cannot read fails at *their* edge, by the backend's
  policy. That is the system working.

Adding a backend is one match arm that shells out to its CLI (`vault`, `op`,
`aws secretsmanager`, …). Backends must be explicitly known — refs never
execute arbitrary commands, because message bodies are attacker-controlled.

## Beads: the board's upstream

The same delegation applies to work. Beads (`bd`) is the agent-native
tracker — local-first, git-distributed, a dependency graph with ready-work
detection — and it owns planning, priorities, dependencies, and long-term
memory across sessions. The board owns the agent mechanics — atomic take,
holder-checked completion — because N racing agents need filesystem
atomicity. `murmur task sync beads` reconciles the two bidirectionally and
idempotently: only *ready* beads pull onto the board (`bd ready` is
dependency-aware — the board must never offer an agent blocked work),
keeping their own ids, and local transitions push back (take → in_progress
with the agent as assignee, done → closed with attribution, drop → open).
A `synced_state` field in the task file records what beads has already been
told, so pushes never repeat and re-pulls never duplicate.

Murmur shells out to the `bd` CLI — the same move as `herdr` and ssh: no
SDK, no daemon, `.beads/` internals never touched. The content rule that
makes the seam real: anything a future session needs (decisions, discovered
work, dependency links) goes to beads; anything only this conversation
needs flows through murmur and is consumed. The stack is three timescales —
beads is memory (days–months, rides git), murmur is the nervous system
(minutes–hours), herdr is attention (seconds) — and each layer talks only
to its neighbor. Herdr never learns what a bead is; the idle-wake plugin
closes the loop by pointing an idle pane with an empty inbox at `bd ready`:
herdr notices free attention, murmur routes it, beads supplies the work.

The trajectory: the local board slims toward a staging area for beads —
kept because any process that can `mv` a file can take a task, even with no
`bd` installed. Other trackers (Linear, GitHub Issues, Jira) remain the
same adapter shape if someone needs one; beads is the default because it
shares murmur's thesis — state in files, transport you already trust (git),
no server. It also means a distributed fleet needs no new infrastructure:
work replicates with `git push/pull`, chatter replicates with
`murmur sync <host>` over ssh, and herdr stays local to each machine.

## Remote: replication, not networking (roadmap)

Local murmur works because the state *is* the medium. Remote means no shared
medium, so remote murmur is a **replication** problem: how do two `.murmur/`
directories reconcile? The data model is already replication-friendly —
messages are write-once with unique ids (idempotent re-delivery), presence is
last-writer-wins, the log is append-only.

Data-model changes required regardless of transport:

1. **Node identity**: a keypair per machine; message ids become
   `ts-node-seq`; addressing grows `agent@node` (bare names still work when
   unambiguous).
2. **Per-node logs**: `log.jsonl` → `log/<node>.jsonl`, so appends never
   conflict and sync state is a simple vector ("seen laptop up to seq 481").
3. **Tombstones**: consuming a message writes a tombstone instead of only
   deleting, so consumed mail does not resurrect on sync.
4. **Deterministic task-conflict rule**: no atomic rename exists across
   machines without consensus (CAP). Under partition, two nodes may take the
   same task; on sync the conflict is detected and the lower node id keeps
   it — the loser auto-drops with an explanation. Occasional duplicated work
   is the accepted price; claims degrade the same way (advisory gets more
   advisory, which TTLs already express).

Transport phases:

- **Phase 0 (built)**: any synced filesystem — NFS, sshfs, Syncthing, a
  shared container volume — or `murmur sync <path>` against a directory both
  machines can see.
- **Phase 1 — `murmur sync <host>` (built)**: one-shot anti-entropy over ssh,
  the way git rides ssh: `ssh host murmur sync --stdio`, exchange
  seen-vectors (per-node line counts — the line number is the sequence
  number), stream missing entries, merge state, rebuild inboxes with
  tombstone suppression. No daemon, no ports, no new auth surface; trust is
  git-pull-style (syncing with a host means trusting it). Relay works
  because logs are per-origin: A↔B, B↔C gives C everything from A.
  Per-entry signatures for *transitive* provenance (trusting A's entries
  relayed via B without trusting B) arrive with phase 2's keypairs; over
  direct ssh, the peer is already authenticated by ssh itself.
- **Phase 1.5 — peers (built)**: `.murmur/peers` lists targets and murmur
  syncs opportunistically — forced after `send`, debounced (30s default) on
  `inbox`, in the hook, and in the idle-wake plugin — so convergence needs
  no human and still no daemon. Best-effort, non-interactive ssh
  (BatchMode, 3s connect timeout): a dead peer can warn, never hang a hook.
  One-way reachability suffices: every sync exchanges both directions.
- **Phase 2 — `murmur peer`** (*deliberately deferred*: harness breadth
  beats push latency — at agent timescales a per-turn phase-1 sync is live
  enough, and no competitor pressure exists on this axis): a per-machine
  replicator daemon (QUIC, LAN discovery, ticket invites) for push-latency
  sync, bound to private interfaces only. The rule that keeps it from becoming v1's orchestrator:
  **servers own state; peers own copies.** The daemon is a dumb replicator —
  kill it and nothing is lost; the directory is still the truth; phase 1
  still works without it. No component's death may lose data.

## Non-goals

- Spawning, scheduling, or supervising agents (v1's `spawn` was a mistake).
- Prompts, roles, workflows, or any orchestration policy.
- Public-internet endpoints, TLS termination, token formats.
- Storing secret values, ever.
- Exactly-once semantics across machines. Physics says no; we say
  "deterministic conflict resolution" and mean it.
