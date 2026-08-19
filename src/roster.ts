// The roster: every cloud agent, live, in one pane. Inside herdr, enter
// (or 'a' for all ACTIVE) fans out attach panes via the herdr CLI; outside
// herdr, enter drops into an inline attach. Non-TTY prints once and exits
// so the roster is scriptable.

import type { CloudAgent, Provider } from "./provider.js";
import { inside, openAttachPane } from "./herdr.js";
import { attach } from "./attach.js";

const CLEAR = "\x1b[2J\x1b[H";
const POLL_MS = Number(process.env.HERDR_CURSOR_POLL_MS ?? 5000);

function row(a: CloudAgent): string {
  const status = a.status.padEnd(9);
  const name = a.name ?? "";
  const where = a.prUrl ?? a.branch ?? "";
  return `${a.id.padEnd(14)} ${status} ${name}${where ? `  (${where})` : ""}`;
}

export async function roster(p: Provider): Promise<void> {
  if (!process.stdout.isTTY || !process.stdin.isTTY) {
    for (const a of await p.list()) process.stdout.write(row(a) + "\n");
    return;
  }

  let agents = await p.list();
  let sel = 0;
  let note = "↑/↓ select · enter attach · a panes for all ACTIVE · r refresh · q quit";
  let inlineAttach: string | null = null;

  const render = () => {
    process.stdout.write(CLEAR);
    process.stdout.write(`cursor cloud agents (${agents.length})\n\n`);
    agents.forEach((a, i) => {
      process.stdout.write((i === sel ? "▸ " : "  ") + row(a) + "\n");
    });
    process.stdout.write(`\n${note}\n`);
  };
  const refresh = async () => {
    agents = await p.list();
    if (sel >= agents.length) sel = Math.max(0, agents.length - 1);
    render();
  };

  const timer = setInterval(() => void refresh().catch(() => {}), POLL_MS);
  process.stdin.setRawMode(true);
  process.stdin.resume();
  render();

  await new Promise<void>((done) => {
    process.stdin.on("data", (buf) => {
      const k = buf.toString();
      if (k === "q" || k === "\x03") return done();
      if (k === "\x1b[A" || k === "k") sel = Math.max(0, sel - 1);
      else if (k === "\x1b[B" || k === "j") sel = Math.min(agents.length - 1, sel + 1);
      else if (k === "r") return void refresh().catch(() => {});
      else if (k === "\r" && agents[sel]) {
        if (inside()) {
          openAttachPane(agents[sel].id);
          note = `opened a pane for ${agents[sel].id}`;
        } else {
          inlineAttach = agents[sel].id; // no herdr: become the attach view
          return done();
        }
      } else if (k === "a") {
        const active = agents.filter((x) => x.status.toUpperCase() === "ACTIVE");
        if (!inside()) note = "not inside herdr — no panes to open (run inside a herdr pane)";
        else if (active.length === 0) note = "no ACTIVE agents";
        else {
          active.forEach((x) => openAttachPane(x.id));
          note = `opened ${active.length} pane(s)`;
        }
      }
      render();
    });
  });

  clearInterval(timer);
  process.stdin.setRawMode(false);
  process.stdin.pause();
  process.stdout.write(CLEAR);
  if (inlineAttach) await attach(p, inlineAttach);
}
