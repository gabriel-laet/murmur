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

- **Phase 0 (works today)**: any synced filesystem — NFS, sshfs, Syncthing, a
  shared container volume. Needs only the per-node log split.
- **Phase 1 — `murmur sync <host>`**: one-shot anti-entropy over ssh, the way
  git rides ssh: `ssh host murmur sync --stdio`, exchange seen-vectors,
  stream missing entries. No daemon, no ports, no new auth surface. Hooks
  trigger a sync each agent turn — at agent timescales, that is live enough.
- **Phase 2 — `murmur peer`**: a per-machine replicator daemon (QUIC,
  LAN discovery, ticket invites) for push-latency sync, bound to private
  interfaces only. The rule that keeps it from becoming v1's orchestrator:
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
