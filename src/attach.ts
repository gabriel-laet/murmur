// Attach entry point: Ink TUI on a terminal; piped callers get the latest
// run streamed once as plain text and a clean exit.
//
// Why this exists at all: Cursor's CLI can *hand off* to a cloud agent
// (`&` prefix) but has no way to list, resume, or stream one — that lives
// only on web/mobile today. The day `agent --resume <cloud-id>` ships,
// this collapses to exec'ing it, and Cursor's own TUI takes over.

import type { Provider } from "./provider.js";
import { reportState, statusToHerdrState } from "./herdr.js";
import { runAttach } from "./ui.js";

export async function attach(p: Provider, id: string): Promise<void> {
  if (process.stdout.isTTY && process.stdin.isTTY) {
    await runAttach(p, id);
    return;
  }
  // piped: stream the latest run once, no prompt
  const agent = await p.get(id);
  process.stdout.write(`cursor cloud agent ${agent.id}${agent.name ? ` — ${agent.name}` : ""}\n`);
  process.stdout.write(
    `status ${agent.status}${agent.branch ? `  branch ${agent.branch}` : ""}${agent.prUrl ? `  pr ${agent.prUrl}` : ""}\n\n`,
  );
  reportState(statusToHerdrState(agent.status));
  const runId = await p.latestRunId(id);
  if (runId) {
    reportState("working");
    await p.stream(id, runId, (e) => {
      if (e.kind === "done") process.stdout.write("\n[run finished]\n");
      else if (e.kind === "status") process.stdout.write(`· ${e.text}\n`);
      else process.stdout.write(e.text.endsWith("\n") ? e.text : `${e.text}\n`);
    });
  }
  reportState("idle");
}
