# murmur

Cross-harness coordination for AI agents, as a directory of files.

Your Claude Code, Codex, Gemini, and Grok sessions work in the same repo but
can't talk to each other. Murmur gives them durable inboxes, presence, a
shared task board with atomic work-stealing, advisory file claims, and
secret references — all in one `.murmur/` directory, on the only transport
every agent already shares: the filesystem. A message is a file renamed into
the recipient's inbox; it waits until they read it. No daemon, no sockets,
no ports. Inspect everything with `ls` and `cat`.

## How it works

The format is the protocol:

```text
.murmur/
  agents/<name>.json               presence: pid, cwd, joined_at, last_seen
  inbox/<name>/*.json              one file per pending message, oldest first
  claims/*.json                    advisory file claims with TTLs
  tasks/{todo,doing,done}/*.json   a task's state is its directory
  log/<node>.jsonl                 append-only record of every message
  peers                            sync targets, one per line (machine-local)
  tmp/                             staging for atomic renames
```

Sending is `write to tmp/` + `rename into inbox/` — atomic, so a reader
never sees a half-written message. Taking a task is
`rename(todo/X, doing/X)` — N racing agents get exactly one winner. Any
process that can `mv` participates with no murmur installed.

## The stack

Murmur is the middle of a three-layer stack and deliberately overlaps with
neither neighbor:

| Layer | Owns | Never does |
| --- | --- | --- |
| [beads](https://github.com/steveyegge/beads) (`bd`) | planning: issues, dependencies, priorities, memory | run or message agents |
| **murmur** | coordination: inboxes, presence, atomic take, claims, secret refs | plan work, supervise processes |
| [herdr](https://herdr.dev) | execution: live panes, agent lifecycle, the screen | know what a bead is, store state |

Murmur talks to its neighbors only by shelling out to their CLIs (`bd`,
`herdr`, and `git`/`gh` for the merge queue) — never their internals. Kill
any layer and the other two keep working.

## Install

```bash
cargo install --path .
murmur setup          # wire this repo; idempotent, never clobbers config
```

Setup writes two tiers of integration:

- **Hooks** (Claude Code, `.claude/settings.json`): the passive, enforced
  tier. SessionStart joins and reports peers and the board; PreToolUse
  injects pending mail and denies edits to files claimed by others; Stop
  blocks ending a turn with unread mail; SessionEnd leaves.
- **AGENTS.md**: the written contract for every other harness — Codex,
  Gemini, Grok, OpenCode, anything with a shell — coordinating through the
  murmur CLI. The CLI is the protocol; there is no per-harness config.

It also seeds `FLEET.md` (the fleet roster, see below) and installs the
Herdr idle-wake plugin when Herdr is present.

Identity: `--as <name>` beats `$MURMUR_AGENT` beats the Herdr pane's agent
name. Panes spawned by `murmur start` arrive pre-named.

## Messaging and presence

```bash
murmur join backend                                   # optional; sending also registers you
murmur send frontend "API is ready" --as backend      # waits in their inbox
murmur send '*' "rebasing main" --as backend          # broadcast
murmur send db "schema final?" --as backend --reply   # block until answered
murmur inbox --as frontend --wait                     # read mail (consumes it)
murmur who                                            # up / idle / gone / remote
murmur watch                                          # live feed for the human
```

Replies correlate by message id (`--reply-to <id>`); an unanswered question
degrades into a plain durable message. In `who`, `idle` means recently seen
with no live process — agents touch presence per command, so idle is not
dead. State lives in `.murmur/`, found like `.git` by walking up (override
with `MURMUR_DIR`).

## Tasks: beads plans, the board executes

Beads owns what exists; murmur owns who holds the lock. The board is a
projection of `bd ready`, never a second tracker.

```bash
murmur task add "write auth tests" --as lead
murmur task take <id> --as worker-1        # the task your lead assigned (preferred)
murmur task take --as worker-1             # or: the oldest open leaf
murmur task done <id> --as worker-1        # or: task drop
murmur task sync beads --parent bd-a1b2    # scoped pull + push, idempotent
```

The rules, enforced by the sync adapter:

- Only *ready* leaves land on the board. A parent of a ready bead is never
  takeable — not even when it is the only file on the board.
- A bead closed in beads ends the murmur task no matter who holds the take;
  the ex-holder gets mail.
- A beads assignee beats a conflicting local take (forced drop, mailed).
- Pulls are scoped. Past ~20 ready leaves an unscoped `task sync beads`
  pulls nothing and says to pass `--parent <epic>`, `--label <l>`, or
  `--all` — one agent following a generic recipe cannot flood the board.

No `bd` installed? The board works standalone.

## Herds

`murmur start` puts the work on the board (a goal string becomes a bead
when `bd` is around) and, if Herdr is running, opens panes, starts named
agents, and hands each a brief. After that, coordination is the board and
inboxes — murmur never supervises.

```bash
murmur plan bd-a1b2 --kind claude            # plan-first: one lead, no workers yet
murmur start bd-a1b2 --kind grok             # lead + worker
murmur start bd-a1b2 --kind claude,codex=2   # mixed: claude leads, codex works
murmur start bd-a1b2 --kind grok --worktree  # one git worktree per agent
murmur start bd-a1b2 --board oficina         # this herd gets its own bus
murmur start "fix claim TTLs" --no-herdr     # board only; prints how to join
murmur stop                                  # close workspace, presence, worktrees
```

**Plan first.** `murmur plan` stands up a single lead briefed to explore,
slice the goal into beads (`bd create` + `bd dep add`), then summon its own
workers with `murmur start --bead <epic>` from its pane. When a *named
agent* runs `start`, the caller becomes the lead and every requested kind
spawns as its worker — no rival lead. A human's plain shell spawns a lead
as before.

**Isolation.** `--board <name>` gives the herd a private store
(`.murmur-<name>/`; panes get `MURMUR_DIR` automatically), so two waves on
one machine never mix agents, mail, or tasks. `murmur clean --stale` prunes
what a finished wave left behind without touching inboxes or logs.

**Worktrees.** With `--worktree`, each agent works in its own checkout
(sibling of the repo, branch `herd/<slug>/<name>`) and never touches yours;
the lead's branch is the integration branch. A monorepo whose checkouts
need installs or symlinks brings its own helper:
`--worktree-cmd 'pnpm worktree:new'` runs per agent with
`MURMUR_WORKTREE_{DIR,BRANCH,NAME}` set — murmur keeps the location and
branch, the helper owns the contents. Claims are keyed by repo identity
plus relative path, so the same file claimed from two worktrees collides
like it should. `--hub <path>` names the files everyone converges on: each
brief carries them and `restack` flags branches touching them.

**The merge queue.** `murmur restack`, run from the integration checkout,
merges each worker branch in one at a time — merges, never rebases; worker
branches are other people's live checkouts — gating each merge on
`--cmd 'pnpm test'`, holding branches whose PR checks fail (when `gh` is
present), and stopping with the conflicting files on the first conflict.
`murmur pr status` is one snapshot of every herd branch's PR: number,
state, check rollup.

**Liveness.** The first brief waits for a live prompt (not just a shell),
so workspace-trust dialogs stop eating it. `murmur poke <name> "..."`
revives a finished pane before prompting — reach a listening model or fail
loudly, never report success on a corpse. The Herdr plugin does the same on
idle-wake: new mail or fresh ready beads restart a done pane before the
nudge.

## The fleet

Different models are good at different work — that's what `FLEET.md` holds:
a short, human-curated table of each kind's strengths and cost. Murmur
never parses it for routing; the lead's brief carries it verbatim and the
lead routes. Two commands read it as data:

- `murmur doctor` lints the roster against the machine: herdr up, kind
  binaries on PATH, cloud keys present, plus one live call to each cloud
  backend's API — a revoked key or spent quota shows up before a herd
  needs it.
- `murmur fleet` shows what this machine actually launched (24h / 7d, per
  kind). Murmur can't see provider quotas, but it counts its own starts,
  and the lead's brief carries the 24h tally so a planner sizes the herd
  against what's already been burned.

### Cloud kinds

Until Herdr owns provider-hosted agents, `cloud:<backend>` bridges the gap
(today: `cloud:cursor`, via `CURSOR_API_KEY`). A cloud agent never touches
the bus — no inbox, no board, no claims, no worktree: it gets the brief as
its launch prompt, works on the provider's VM, and comes back through the
git host as a PR referencing the bead. Follow up explicitly with
`murmur cloud status|prompt|list`. A cloud kind can't lead a mixed herd.

## Remote: sync, not servers

Murmur crosses machines by reconciling `.murmur/` directories — no ports,
no daemon. Syncs are idempotent, relay through intermediate nodes, and
consumed mail leaves tombstones so it never resurrects.

```bash
murmur sync dev@buildbox:work/myrepo   # over ssh — your existing keys
murmur sync /mnt/shared/myrepo         # or any path both machines see
```

List peers in `.murmur/peers` and sync runs itself: forced after `send`,
debounced on `inbox`, in the hook, and in the idle-wake. Best-effort and
non-interactive; a dead peer warns once per interval. There is no atomic
rename across machines: a doubly-taken task resolves deterministically on
sync and the loser's `task done` fails visibly. Syncing with a host means
trusting it, like `git pull`. Durable cross-machine work rides git anyway
(beads in the repo, PRs on the host); sync carries the chatter.

## Secrets: references, never values

The bus is plaintext files, so values never touch it — agents share
references, which grant nothing by themselves:

```bash
murmur send bob "creds: secret://infisical/proj-123/dev/DATABASE_URL" --as alice
murmur secret exec DATABASE_URL=secret://infisical/proj-123/dev/DATABASE_URL -- psql
```

Resolution happens only inside `secret exec`, at the receiver's edge, with
the receiver's own backend identity (`INFISICAL_TOKEN` etc.); the value
goes into the child command's environment and nowhere else — never stdout,
never a context window. Hooks never auto-resolve; refs arrive labeled
"never resolve this into your context". `secret://env/<VAR>` covers the
local case.

## File claims

Advisory locks with TTLs (default 60 min) so a crashed agent never
deadlocks anyone. Automatic around every Edit/Write with the hook
installed; within a worktree herd, branch isolation plus `restack` is the
real serializer and claims are the guardrail.

```bash
murmur claim src/auth.rs --as backend    # peers get an error + who holds it
murmur release src/auth.rs --as backend
murmur claims
```

## Design rules

The binary has two layers, held to different standards:

- **The kernel** (store, mail, tasks, claims, sync) is mechanism only. It
  never spawns, schedules, prompts, routes, or parses FLEET.md. If a change
  makes the kernel know more about planning than `bd ready`, reject it.
- **Userland** (`start`, `plan`, `restack`, `pr`, `doctor`, briefs) is
  deliberately opinionated policy over the kernel plus `bd`, `herdr`,
  `git`, and `gh` — always shelling out to CLIs, deletable without touching
  the kernel, and never a daemon.

Non-goals: a scheduler or DAG executor, process supervision, routing logic
that parses the roster, public-internet endpoints, storing secret values,
exactly-once semantics across machines.

## Command reference

```bash
murmur send <to> [msg]     # deliver ('*' = broadcast; stdin if no msg)
                           #   --reply blocks; --reply-to <id> answers
murmur inbox               # read + consume your mail (--wait, --peek, --json)
murmur who                 # agents and liveness (up / idle / gone / remote)
murmur status              # who is up + the open board
murmur join [name]         # register presence, see peers
murmur leave               # deregister, release claims
murmur claim <path>        # advisory claim (--ttl secs)
murmur release <path>      # release a claim
murmur claims              # list active claims
murmur task add|list|take|done|drop   # the board (take [id] = assigned task)
murmur task sync beads     # reconcile with beads: --parent/--label/--all
murmur plan [goal|bead]    # one planning lead that summons its own herd
murmur start [goal|bead]   # bead -> board -> Herdr herd
                           #   --kind claude,codex=2  --board <name>
                           #   --worktree [--worktree-cmd '<helper>']  --hub <path>
murmur stop [--board <n>]  # close the herd's workspace + worktrees
murmur restack [--cmd]     # lead: merge worker branches one at a time
murmur pr status           # herd branches' PRs: number, state, checks (gh)
murmur poke <name> <msg>   # prompt a Herdr agent (revives finished panes)
murmur fleet               # roster + observed agent starts (24h / 7d)
murmur doctor              # can this machine launch the roster right now?
murmur cloud status|prompt|list       # follow up on provider-hosted agents
murmur secret exec NAME=<ref> -- cmd  # resolve refs into a command's env
murmur sync [peer]         # reconcile with a peer .murmur (ssh or path)
murmur log [-n N]          # recent message history
murmur watch               # follow all traffic live (--all for history)
murmur clean               # prune dead agents + expired claims
                           #   --stale: old herd leftovers; --all: rm .murmur
murmur setup               # hooks + AGENTS.md + FLEET.md + Herdr plugin
murmur hook                # Claude Code hook adapter (used by setup)
murmur herdr               # Herdr plugin adapter (used by the plugin)
```

## License

MIT
