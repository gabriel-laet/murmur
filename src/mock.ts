// A canned provider so the roster, attach view, panes, and state bridge are
// all testable end to end with no API key and no quota — same move as
// murmur's fake-curl tests.

import type { CloudAgent, Provider, RunEvent } from "./provider.js";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export function mock(
  delayMs = Number(process.env.HERDR_CURSOR_MOCK_DELAY ?? 150),
): Provider {
  const agents: CloudAgent[] = [
    { id: "bc-mock-1", status: "ACTIVE", name: "add dark mode", branch: "cursor/dark-mode" },
    { id: "bc-mock-2", status: "ACTIVE", name: "fix flaky tests", branch: "cursor/flaky-tests" },
    { id: "bc-mock-3", status: "FINISHED", name: "readme pass", prUrl: "https://github.com/acme/demo/pull/7" },
  ];
  let runSeq = 0;

  const get = async (id: string): Promise<CloudAgent> => {
    const a = agents.find((x) => x.id === id);
    if (!a) throw new Error(`no such agent: ${id}`);
    return a;
  };

  return {
    async list() {
      return agents;
    },
    get,
    async latestRunId(id) {
      await get(id);
      return `run-${id}-0`;
    },
    async stream(id, _runId, onEvent) {
      await get(id);
      const lines: RunEvent[] = [
        { kind: "status", text: "cloning repo…" },
        { kind: "message", text: "Reading src/auth.ts and the failing test." },
        { kind: "message", text: "Patched the token refresh; running the suite." },
        { kind: "status", text: "tests: 42 passed" },
        { kind: "message", text: "Pushed branch and opened a draft PR." },
      ];
      for (const e of lines) {
        await sleep(delayMs);
        onEvent(e);
      }
      await sleep(delayMs);
      onEvent({ kind: "done", text: "" });
    },
    async followup(id, _text) {
      await get(id);
      return { runId: `run-${id}-${++runSeq}` };
    },
  };
}
