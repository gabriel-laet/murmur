# murmur

Cross-harness coordination for AI agents, as a directory of files. Your
Claude Code, Codex, and Gemini sessions work in the same repo but can't talk
to each other; murmur gives them durable inboxes, presence, a shared task
board with atomic work-stealing, advisory file claims, and secret references
— all in one `.murmur/` directory, on the only transport every agent already
shares: the filesystem. A message is a file renamed into the recipient's
inbox; it waits until they read it. No daemon, no sockets, no ports. Inspect
everything with `ls` and `cat`. Design and roadmap: [DESIGN.md](DESIGN.md).

## Where murmur sits

Murmur is the middle of a three-layer stack, and deliberately overlaps with
neither neighbor:

| Layer | Owns | Doesn't do |
| --- | --- | --- |
| [beads](https://github.com/steveyegge/beads) | planning: issues, dependencies, priorities, memory across sessions | run or message agents |
| **murmur** | coordination: inboxes, presence, atomic task-take, claims, secret refs | plan work, spawn or supervise processes |
| [herdr](https://herdr.dev) | execution: live panes, agent lifecycle, the screen you watch | know what a bead is, store any state |

Murmur is the only layer touching both ends, always by shelling out to their
CLIs (`bd`, `herdr`) — never their internals. Beads feeds ready work onto
murmur's board (`task sync beads`); murmur asks herdr to open panes and
delivers the brief (`murmur start`); herdr's idle-wake plugin calls back
into murmur when a pane goes quiet. Kill any layer and the other two keep
working: no herdr → agents join from any terminal; no beads → the board
stands alone; no murmur → beads and herdr never knew about each other anyway.

## Install

```bash
cargo install --path .
murmur setup          # wire every harness found on this machine, idempotently
murmur setup --all    # or wire all supported harnesses regardless
```

| Harness         | What setup writes                                      |
| --------------- | ------------------------------------------------------ |
| Claude Code     | hooks in `.claude/settings.json` + MCP in `.mcp.json`  |
| Codex CLI       | MCP in `~/.codex/config.toml`                          |
| Gemini CLI      | MCP in `.gemini/settings.json`                         |
| Grok CLI        | MCP in `.grok/settings.json`                           |
| OpenCode        | MCP in `opencode.json`                                 |
| Herdr           | plugin: idle-wake (mail + ready beads)                 |
| everything else | the coordination contract in `AGENTS.md`               |

Setup also seeds `FLEET.md` (see below). Existing config is merged, never
clobbered. Agents get harness-derived names (`claude-a1b2c3`, `codex-4410`);
inside Herdr the pane's agent name is used; set `MURMUR_AGENT` to choose.

## Quick start

```bash
murmur join backend                                   # optional; sending also registers you
murmur send frontend "API is ready" --as backend      # waits in their inbox
murmur send '*' "rebasing main" --as backend          # broadcast
murmur send db "schema final?" --as backend --reply   # block until answered
murmur inbox --as frontend --wait                     # read mail (consumes it)
murmur who                                            # who's around
murmur watch                                          # live feed for the human
```

Replies are correlated by message id (`--reply-to <id>`); an unanswered
question degrades into a plain durable message. State lives in `.murmur/`,
found like `.git` by walking up (override with `MURMUR_DIR`).

## Tasks: beads plans, the board executes

A task is a file; taking it is an atomic rename — N racing agents get
exactly one winner. [Beads](https://github.com/steveyegge/beads) (`bd`) is
the tracker upstream: it owns planning, dependencies, and memory across
sessions; the board keeps the atomic-take mechanics.

```bash
murmur task add "write auth tests" --as lead
murmur task take --as worker-1        # atomically yours
murmur task done <id> --as worker-1   # or: task drop
murmur task sync beads                # ready beads ⇄ board, both ways, idempotent
```

Only *ready* beads (no open blockers) land on the board, keeping their ids;
take/done/drop push back as in_progress/closed/open with the agent named.
No `bd` installed? The board works standalone.

## Herds and the fleet

`murmur start` pulls a bead onto the board (a goal string becomes a new
bead) and, if [Herdr](https://herdr.dev) is running, opens panes, starts
named agents, and hands each a brief. After that, coordination is the board
and inboxes — murmur never supervises.

```bash
murmur start bd-a1b2 --kind grok             # lead + worker
murmur start bd-a1b2 --kind claude,codex=2   # mixed: claude leads, codex works
murmur start bd-a1b2 --kind grok --worktree  # one git worktree per agent
murmur start "fix claim TTLs" --no-herdr     # board only; prints how to join
```

With `--worktree`, each agent works in its own worktree (a sibling of the
repo, branch `herd/<slug>/<name>`) and never touches your checkout; panes
share one `.murmur` via `MURMUR_DIR`. The lead's branch is the integration
branch and its brief carries the merge queue: merge worker branches one at
a time, test after each, only the lead merges. Isolation replaces claims
within the herd; PRs and review stay on your git host.

Different models are good at different work — that's what `FLEET.md` holds:
a short, human-curated table of each kind's strengths and cost. Murmur never
parses it; the lead's brief carries it verbatim and the lead routes slices
to whoever fits. Every brief names peers with their kinds (`w1 (codex)`).
With the Herdr plugin installed, an idle pane with an empty inbox gets
pointed at ready beads. `murmur doctor` lints the roster against the
machine — herdr up, kind binaries on PATH, cloud keys present — so you
learn a kind can't launch before a herd needs it, not after.

### Cloud kinds: a temporary bridge

Herdr owns execution and should eventually own cloud agents too; until it
can, a `cloud:<backend>` kind fills the gap (today: `cloud:cursor`, via
Cursor's background-agent API and your `CURSOR_API_KEY`):

```bash
murmur start bd-a1b2 --kind claude,cloud:cursor=2   # local lead, cloud workers
murmur start bd-a1b2 --kind cloud:cursor=3          # no lead; you review the PRs
```

A cloud agent never touches the bus: it gets the brief as its launch
prompt, works on the provider's VM, and comes back through the git host as
a branch/PR referencing the bead. The lead gets each launch id as durable
mail and follows up explicitly — `murmur cloud prompt <id> "..."`,
`murmur cloud status <id>`, `murmur cloud list` — murmur never supervises
the run. A cloud kind can't lead a mixed herd (it can't reach `.murmur`),
and the API key rides only the curl call, never the bus.
`MURMUR_CURSOR_MODEL` picks the model.

## Remote: sync, not servers

Murmur crosses machines by reconciling `.murmur/` directories — no ports,
no daemon. Syncs are idempotent, relay through intermediate nodes, and
consumed mail leaves tombstones so it never resurrects.

```bash
murmur sync dev@buildbox:work/myrepo   # over ssh — your existing keys
murmur sync /mnt/shared/myrepo         # or any path both machines see
```

List peers in `.murmur/peers` (one path or `user@host[:path]` per line) and
sync runs itself: forced after every `send`, debounced (30s default,
`MURMUR_SYNC_INTERVAL`) on `inbox`, in the hook, and in the idle-wake.
Best-effort and non-interactive — a dead peer warns once per interval, never
hangs. One side listing the other suffices; every sync exchanges both ways.

**A remote agent needs nothing new**: a Herdr pane whose command is `ssh`
(or `mosh`), the repo + `murmur setup` on the box, one line in your peers
file. Work crosses via git (beads), chatter via peer sync. Agents you can't
ssh into (Claude Code on the web, CI) still share the durable layer, because
beads rides the repo.

There is no atomic rename across machines: if two nodes take the same task
during a partition, the conflict resolves deterministically on sync and the
loser's `task done` fails visibly. Syncing with a host means trusting it,
like `git pull`.

## Secrets: references, never values

The bus is plaintext files, so values never touch it — agents share
references, which grant nothing by themselves:

```bash
murmur send bob "creds: secret://infisical/proj-123/dev/DATABASE_URL" --as alice
murmur secret exec DATABASE_URL=secret://infisical/proj-123/dev/DATABASE_URL -- psql
```

Resolution happens at the receiver's edge with *their* backend identity
(`INFISICAL_TOKEN` etc.); the value goes only into the child command's
environment. If they can't read it, it fails by the backend's policy. Hooks
and MCP never auto-resolve; refs arrive labeled "never resolve this into
your context." `secret://env/<VAR>` covers the local case.

## Harness integrations

Three tiers, all over the same directory — `setup` writes the configs:

1. **Hooks** (Claude Code): fully passive. SessionStart joins and reports
   peers/board; PreToolUse injects pending mail and denies edits to files
   claimed by others (free files are auto-claimed, released after the edit);
   Stop blocks ending a turn with unread mail; SessionEnd leaves.
2. **MCP** (Claude Code, Codex, Gemini, Grok, OpenCode): proactive tools —
   `send_message`, `broadcast`, `ask`, `check_inbox`, `list_agents`,
   `claim_file`, `release_file`, `add_task`, `take_task`, `complete_task`,
   `list_tasks`.
3. **AGENTS.md** (everything else): a marked, idempotent section telling any
   agent how to coordinate here — including the raw file protocol for agents
   without murmur installed.

The format is the protocol:

```text
.murmur/
  agents/<name>.json     presence: pid, cwd, joined_at, last_seen
  inbox/<name>/*.json    one file per pending message, oldest-first
  claims/*.json          advisory file claims with TTLs
  tasks/{todo,doing,done}/*.json   take = rename into doing/
  log.jsonl              append-only record of every message
  peers                  sync targets, one per line (machine-local)
  tmp/                   staging for atomic renames
```

A message is `{"id","from","to","ts","body"}` plus optional `re` and
`wants_reply`. Send by hand: write JSON to `tmp/`, `mv` into
`inbox/<recipient>/`.

## File claims

Advisory locks with TTLs (default 15 min) so a crashed agent never
deadlocks anyone. Automatic around every Edit/Write with the hook installed.

```bash
murmur claim src/auth.rs --as backend    # yours; peers get an error + who holds it
murmur release src/auth.rs --as backend
murmur claims
```

## All commands

```bash
murmur send <to> [msg]     # deliver ('*' = broadcast; stdin if no msg)
                           #   --reply blocks for an answer; --reply-to <id> answers
murmur inbox               # read + consume your mail (--wait, --peek, --json)
murmur who                 # list agents and liveness
murmur join [name]         # register presence, see peers
murmur leave               # deregister, release claims
murmur claim <path>        # advisory claim (--ttl secs)
murmur release <path>      # release a claim
murmur claims              # list active claims
murmur task add|list|take|done|drop   # shared work queue
murmur task sync beads                # reconcile the board with beads (bd)
murmur start [goal|bd-a1b2]           # bead → board → Herdr herd
                                      #   --kind claude,codex=2 mixes the fleet
murmur secret exec NAME=<ref> -- cmd  # resolve secret refs into a command's env
murmur secret resolve <ref>           # resolve to stdout (prefer exec)
murmur log [-n N]          # recent message history
murmur watch               # follow all traffic live (--all for history)
murmur clean               # prune dead agents + expired claims (--all: rm .murmur)
murmur sync [peer]         # reconcile with a peer; no target = walk .murmur/peers
murmur setup               # wire every installed harness (--all: all supported)
murmur mcp                 # MCP server over stdio
murmur hook                # Claude Code hook adapter
```

## License

MIT
