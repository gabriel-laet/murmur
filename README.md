# herdr-cursor

Cursor cloud agents as first-class [herdr](https://herdr.dev) panes: a live
roster, a streaming attach view per agent, and a state bridge so herdr's
sidebar (and anything listening to it, like murmur's idle-wake) treats a
cloud agent like any other pane.

**Why this exists.** Cursor's CLI can *hand off* work to a cloud agent
(`&` prefix), but there is no CLI path to list, resume, follow up, or
stream one — that lives only on web/mobile today. And herdr can't own
cloud agents yet: they have no terminal to hold. This tool fills both gaps
with the provider API, and is deliberately temporary — the day herdr grows
native cloud panes, or Cursor's CLI grows `agent --resume <cloud-id>`,
this repo gets archived.

## Install

```bash
npm run build && npm i -g .    # puts `herdr-cursor` on PATH
export CURSOR_API_KEY=...      # Cursor dashboard → API keys
```

## Use

```bash
herdr-cursor                   # roster: every cloud agent, live (Ink TUI)
herdr-cursor list --json       # scriptable inventory
herdr-cursor attach bc-abc123  # stream one agent, type follow-ups
herdr-cursor --mock            # all of the above with canned data, no key
herdr-cursor --rest            # zero-dep REST fallback instead of the SDK
```

The default provider is `@cursor/sdk` — `Agent.resume()` attaches to any
`bc-` id (including agents murmur launched over REST) and `run.stream()`
yields typed events, so the attach view renders tool calls (`▸ edit`),
thinking (dimmed), status, and per-turn token usage distinctly. `--rest`
keeps a dependency-free fallback over plain fetch.

In the roster: `↑/↓` select, `enter` attach (inside herdr this opens a new
pane; outside it attaches inline), `a` opens a pane per ACTIVE agent,
`r` refreshes, `q` quits. The list re-polls every 5s
(`HERDR_CURSOR_POLL_MS`).

## Inside herdr

Register `herdr-plugin.toml` with herdr's plugin discovery. You get a
"Cursor Cloud" popup pane and a workspace action opening the roster. Each
attach pane reports `working`/`idle` via
`herdr pane report-agent --source custom:cursor-cloud`, so cloud agents
appear in the sidebar with live state — and idle-wake plugins fire on them
like on any local agent.

## With murmur

[murmur](https://github.com/gabriel-laet/murmur) launches cloud agents
(`murmur start --kind cloud:cursor`) and records launch ids in the lead's
mail; herdr-cursor is the *screen* for them. The seam is one-directional:
this tool reads only the provider API and never touches `.murmur/`.

## Honest caveats

- One active run per Cursor agent: a follow-up while a run streams gets a
  409 (`agent_busy`); wait it out.
- API-created agents are hidden in Cursor's dashboard behind
  Source → SDK; this roster is the honest inventory.
- The REST fallback parses payloads permissively (`src/rest.ts`); the
  live API wraps lists as `{items: [...]}` and nests the create response
  as `{agent, run}` (verified against the real API, 2026-08-19).
- `herdr pane split` flag shape is per current docs; adjust
  `src/herdr.ts` if your herdr version differs.

## Test

```bash
npm test                       # mock provider end to end, no key, no quota
```
