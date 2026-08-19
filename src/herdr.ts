// The herdr bridge. Everything here is best-effort and shells out to the
// herdr CLI (never its socket), the same seam murmur uses: if herdr is
// absent, herdr-cursor still works as a plain terminal tool.

import { spawn } from "node:child_process";

export function inside(): boolean {
  return process.env.HERDR_ENV === "1";
}

export function bin(): string {
  return process.env.HERDR_BIN_PATH ?? "herdr";
}

/** Cloud run states → the two pane states herdr's sidebar knows. */
export function statusToHerdrState(status: string): "working" | "idle" {
  const s = status.toUpperCase();
  return s === "CREATING" || s === "ACTIVE" || s === "RUNNING" ? "working" : "idle";
}

/** Mirror this pane's cloud agent into herdr's sidebar, so idle-wake
 * plugins (murmur's included) fire on cloud agents like any other pane. */
export function reportState(state: "working" | "idle"): void {
  const pane = process.env.HERDR_PANE_ID;
  if (!inside() || !pane) return;
  run([
    "pane", "report-agent", pane,
    "--source", "custom:cursor-cloud",
    "--agent", "cursor",
    "--state", state,
  ]);
}

/** Open a sibling pane streaming one cloud agent. The flag shape follows
 * current herdr docs; adjust here if your herdr version's `pane split`
 * takes its command differently. */
export function openAttachPane(id: string): void {
  run([
    "pane", "split", "--direction", "down", "--no-focus",
    "--env", `CURSOR_AGENT_ID=${id}`,
    "--", process.execPath, process.argv[1], "attach", id,
  ]);
}

function run(args: string[]): void {
  const child = spawn(bin(), args, { stdio: "ignore" });
  child.on("error", () => {}); // no herdr → no bridge, never a crash
}
