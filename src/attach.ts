// One pane, one cloud agent: stream its latest run, then take follow-ups.
//
// Why this exists at all: Cursor's CLI can *hand off* to a cloud agent
// (`&` prefix) but has no way to list, resume, or stream one — that lives
// only on web/mobile today. The day `agent --resume <cloud-id>` ships,
// replace the loop below by exec'ing it and delete most of this file.

import * as readline from "node:readline";
import type { Provider } from "./provider.js";
import { reportState, statusToHerdrState } from "./herdr.js";

export async function attach(p: Provider, id: string): Promise<void> {
  const agent = await p.get(id);
  const title = agent.name ? ` — ${agent.name}` : "";
  process.stdout.write(`cursor cloud agent ${agent.id}${title}\n`);
  process.stdout.write(
    `status ${agent.status}${agent.branch ? `  branch ${agent.branch}` : ""}${agent.prUrl ? `  pr ${agent.prUrl}` : ""}\n\n`,
  );
  reportState(statusToHerdrState(agent.status));

  const runId = await p.latestRunId(id);
  if (runId) await streamRun(p, id, runId);

  if (!process.stdin.isTTY) return; // piped: stream once and exit

  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  for (;;) {
    const text: string = await new Promise((res) => rl.question("follow-up> ", res));
    if (!text.trim() || text.trim() === "/q") break;
    try {
      const { runId } = await p.followup(id, text.trim());
      await streamRun(p, id, runId);
    } catch (e) {
      // 409 agent_busy: one active run per agent — wait for it to finish.
      process.stdout.write(`error: ${(e as Error).message}\n`);
    }
  }
  rl.close();
  reportState("idle");
}

async function streamRun(p: Provider, id: string, runId: string): Promise<void> {
  reportState("working");
  await p.stream(id, runId, (e) => {
    if (e.kind === "done") process.stdout.write("\n[run finished]\n");
    else if (e.kind === "status") process.stdout.write(`· ${e.text}\n`);
    else process.stdout.write(e.text.endsWith("\n") ? e.text : `${e.text}\n`);
  });
  reportState("idle");
}
