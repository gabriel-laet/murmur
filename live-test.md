# Live test: herdr-cursor + murmur against the real Cursor API

Date: 2026-08-19. One agent launched total (authorized). No PRs merged/approved/closed, no cancel endpoints called. All ids truncated to 6-char prefixes; no key material or payload text included.

## Test 1 — herdr-cursor (read-only)

- `npm test` (mock mode): **pass**, 4/4 tests.
- `npm run build`: pass.
- `node dist/main.js list` (real API, GET /v1/agents): **exit 0**, but printed **0 agents**.

**Finding (bug):** the real API returns the agent list wrapped as `{"items": [...]}`. `rest.ts list()` unwraps only `Array | v.agents | v.data`, so it silently maps an empty list. A raw GET /v1/agents (HTTP 200) at the end of the run showed **7 agents: 3 ACTIVE, 4 ARCHIVED**. Fix: add `v.items` to the unwrap chain.

## Test 2 — murmur cloud launch

- `cargo build`: pass (murmur v0.6.0).
- `murmur start "…" --kind cloud:cursor --workers 1`: **exit 1**, error: `could not launch cloud:cursor as w1: cursor returned no agent id`, followed by `no cloud agent launched`.

**Finding (bug — the launch actually succeeded):** the POST to create an agent returned 2xx and created the agent (id prefix `bc-ba3`, status ACTIVE at creation), but the response nests the agent as `{"agent": {"id": ..., "status": ...}, "run": {"id": ..., "status": "CREATING"}}`. murmur looks for a top-level `id`, finds none, and reports the launch as failed even though quota was spent and the agent is running. Fix: read `agent.id` (and optionally `run.id`) from the create response.

- `murmur cloud status bc-ba3…`: **exit 0**, `status: "ACTIVE"`. This works because GET /v1/agents/{id} returns a *flat* agent object with top-level `id`/`status` — only the create response is nested.
- Post-launch `node dist/main.js list`: still 0 (same `items` unwrap bug), so the "+1 count" check could not be observed through herdr-cursor. The raw API list did include the launched agent (prefix `bc-ba3`) among the 7.

## Summary

| Check | Result |
|---|---|
| herdr-cursor mock tests | pass (4/4) |
| herdr-cursor real `list` | exit 0, but wrong: 0 shown vs 7 actual (`items` unwrap bug) |
| murmur cloud launch | agent created (prefix `bc-ba3`), murmur mis-reports failure (nested `agent.id` in create response) |
| murmur `cloud status` | exit 0, status ACTIVE |
| Count +1 via herdr list | not observable (blocked by `items` bug); agent present in raw list |

Both failures are response-shape parsing bugs, not auth or request-payload problems: the request payloads were accepted by the API as-is. Note the launched agent has `autoCreatePR: true` and may open a PR on this repo; it was left untouched.
