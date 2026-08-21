# murmur

The foreman for AI agent waves: beads plans, herdr executes, murmur conducts.

Murmur turns a [beads](https://github.com/steveyegge/beads) plan into a
working herd and a merged branch. It stands up named agents in
[herdr](https://herdr.dev) panes — one git worktree each — hands every
agent a brief, routes assignments onto beads, keeps a merge queue for the
lead, and tears the wave down when the branch is green. It never plans and
it never supervises a process: the plan is beads' job, the panes are
herdr's, and both are hard dependencies murmur drives through their CLIs.

## The stack

| Layer | Owns | Never does |
| --- | --- | --- |
| [beads](https://github.com/steveyegge/beads) (`bd`) | the plan: issues, dependencies, ready detection, assignment, memory | run or message agents |
| **murmur** | the wave: briefs, casting, delivery, the merge queue | plan work, watch processes |
| [herdr](https://herdr.dev) | execution: panes, agent lifecycle, presence, the screen | know what a bead is |

Assignment lives *on the bead* (`in_progress` + assignee); completion *is*
the bead closing. There is no second tracker, no task board, no separate
presence — `murmur who` is a view over herdr's live agents. Murmur's own
state is one small notebook:

```text
.murmur/
  herd.json             the running wave: workspace, agents, worktrees, hubs
  briefs/<name>.txt     each agent's brief, kept for re-delivery
  spool/<name>/*.json   undelivered tells, drained into prompts on idle-wake
```

## Install

```bash
cargo install --path .
murmur setup          # AGENTS.md contract + FLEET.md + playbooks + Herdr plugin
```

Requires a running herdr (panes, presence, delivery) and `bd` for the
plan/assign/done lifecycle — `murmur doctor` checks both. Setup writes
knowledge as repo data, no per-harness config: the **AGENTS.md** contract
any harness with a shell can follow, **FLEET.md** (the human-curated
roster of which model is good at what), and the role playbooks
(`.claude/skills/murmur-lead/SKILL.md`, `murmur-worker/SKILL.md`) —
skill-aware harnesses load them on demand, everything else reads markdown.
The Herdr plugin delivers spooled tells and points idle panes at ready
beads.

## A wave

```bash
murmur plan bd-a1b2 --kind claude            # one lead plans, then summons its herd
murmur start bd-a1b2 --kind grok=3 --worktree
murmur assign bd-a1b2.1 w1 --note "..."      # bead assignee + the worker hears it
murmur tell w2 "status?"                     # delivered now, or spooled for next idle
murmur done bd-a1b2.1 --note "what changed"  # closes the bead, lead hears it
murmur restack --cmd 'pnpm test'             # lead: merge worker branches, gated
murmur pr status                             # every herd branch's PR + checks
murmur status                                # the wave on one screen
murmur stop                                  # workspace, worktrees, done
```

**Plan first.** `murmur plan` starts a single lead briefed to explore,
slice the goal into beads (`bd create` + `bd dep add`), then summon its own
workers with `murmur start --bead <goal>` from its pane — the caller
becomes the lead, every requested kind spawns as its worker, no rival
lead. A goal string instead of a bead id becomes a bead when `bd` is
around; without `bd` it stays a label and "done" is the lead saying so.

**Assignment is the bead.** `murmur assign <bead> <worker>` sets
`in_progress` + assignee and hands the worker its slice as a prompt.
Workers close their own beads (`murmur done`), hand them back
(`murmur drop`), and never grab work on their own — the retro rule
"one assignment, beads owns it" is now the only path.

**Delivery never lies.** `murmur tell` revives a finished pane before
prompting, and spools when nobody is listening — the Herdr idle-wake
plugin drains the spool the moment the pane settles. A pane stuck on a
login or trust dialog ate its brief? Clear it and `murmur tell <name>
--brief` re-delivers the stored brief.

**Worktrees.** With `--worktree`, each agent works in its own checkout
(branch `herd/<slug>/<name>`); the lead's branch is the integration
branch. The notebook anchors to the repo, so every worktree shares it with
no plumbing. Monorepos bring their own helper — `--worktree-cmd 'pnpm
worktree:new'` runs per agent with `MURMUR_WORKTREE_{DIR,BRANCH,NAME,SLOT}`
set. `--hub <path>` names files everyone converges on (briefs carry them,
restack flags them); `--with '<cmd>'` runs a service pane (dev server)
beside each worker — herdr owns the process, murmur never watches it, and
`MURMUR_WORKTREE_SLOT` keys ports so herdmates don't collide.
`--board <name>` gives a wave its own notebook so waves never mix.

**The merge queue.** `murmur restack`, run from the integration checkout,
merges each worker branch one at a time — merges, never rebases — gating
each on `--cmd`, holding branches whose PR checks are red (via `gh`), and
stopping with the conflicting files on the first conflict.

## The fleet

`FLEET.md` is a short, human-curated table of each kind's strengths and
cost. Murmur never parses it for routing — the lead's brief carries it
verbatim (with the machine's recent usage tally from `murmur fleet`) and
the lead routes. `murmur doctor` lints the roster against the machine:
herdr up, kind binaries on PATH, cloud keys present, plus one live call to
each cloud backend's API so a revoked key or spent quota shows up before a
wave needs it.

### Cloud kinds

Until herdr owns provider-hosted agents, `cloud:<backend>` bridges the gap
(today: `cloud:cursor`, via `CURSOR_API_KEY`). A cloud agent can't hear
murmur — no tells, no assignments: it gets the brief as its launch prompt
and comes back through the git host as a PR referencing the goal. Follow
up with `murmur cloud status|prompt|list`. A cloud kind can't lead a mixed
herd.

## Secrets

References, never values: a `secret://...` ref in a prompt grants nothing
by itself. Resolution happens only inside `murmur secret exec NAME=<ref>
-- <cmd>`, at the receiver's edge with the receiver's own backend identity
(`INFISICAL_TOKEN` etc.); the value enters that command's environment and
nowhere else. `secret://env/<VAR>` covers the local case.

## Design rules

Two layers in one binary, held to different standards:

- **The notebook** (briefs, spool, herd snapshot) is mechanism only. If a
  change makes it know more about planning than `bd ready`, or turns the
  spool back into a mailbox agents are taught to poll, reject it.
- **The verbs** (`start`, `plan`, `assign`, `tell`, `restack`, `pr`,
  `doctor`) are deliberately opinionated policy over beads, herdr, git,
  and gh — always shelling out to CLIs, never a daemon, deletable without
  touching the notebook.

Murmur allocates facts (names, slots, hubs) and requests panes; it never
watches what runs in them. Non-goals: a scheduler or DAG executor, process
supervision, routing logic that parses the roster, storing secret values,
its own presence or tracker.

Herdr and beads are required, not optional — that is a deliberate trade:
murmur 0.8 retired the "any agent, any harness, no dependencies" kernel
(file inboxes, task board, claims, ssh sync, hooks) after two field runs
showed the herd coordinating through assignments and prompts, not
mailboxes and work-stealing.

## Command reference

```bash
murmur plan [goal|bead]    # one planning lead that summons its own herd
murmur start [goal|bead]   # goal -> workspace -> agents in worktrees -> briefs
                           #   --kind claude,codex=2  --workers N  --board <name>
                           #   --worktree [--worktree-cmd '<helper>']
                           #   --hub <path>  --with '<service cmd>'
murmur assign <bead> <agent>   # bead assignee + the worker hears the slice
murmur done <bead>         # close with attribution; lead hears it (--note)
murmur drop <bead>         # hand it back; lead told to reassign
murmur tell <agent> <msg>  # deliver into their pane now, or spool for idle
                           #   --brief re-delivers the stored start brief
murmur who                 # herdr's live agents + spool depths (--json)
murmur status              # wave, agents, spool, ready frontier
murmur restack [--cmd]     # lead: merge worker branches one at a time
murmur pr status           # herd branches' PRs: number, state, checks (gh)
murmur fleet               # roster + observed agent starts (24h / 7d)
murmur doctor              # can this machine run the roster right now?
murmur stop [--board <n>]  # close the workspace, remove worktrees
murmur clean               # prune stale spool + briefs (--all: rm .murmur)
murmur cloud status|prompt|list       # follow up on provider-hosted agents
murmur secret exec NAME=<ref> -- cmd  # resolve refs into a command's env
murmur setup               # AGENTS.md + FLEET.md + playbooks + Herdr plugin
murmur herdr               # Herdr plugin adapter (used by the plugin)
```

## License

MIT
