// Roster entry point: Ink TUI on a terminal, plain one-shot table for
// pipes so `herdr-cursor roster | grep ACTIVE` stays scriptable.

import type { CloudAgent, Provider } from "./provider.js";
import { runRoster } from "./ui.js";

function row(a: CloudAgent): string {
  const where = a.prUrl ?? a.branch ?? "";
  return `${a.id.padEnd(14)} ${a.status.padEnd(9)} ${a.name ?? ""}${where ? `  (${where})` : ""}`;
}

export async function roster(p: Provider): Promise<void> {
  if (!process.stdout.isTTY || !process.stdin.isTTY) {
    for (const a of await p.list()) process.stdout.write(row(a) + "\n");
    return;
  }
  await runRoster(p);
}
