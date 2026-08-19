#!/usr/bin/env node
import { mock } from "./mock.js";
import { rest } from "./rest.js";
import { roster } from "./roster.js";
import { attach } from "./attach.js";
import type { Provider } from "./provider.js";

const USAGE = "usage: herdr-cursor [roster | list [--json] | attach <agent-id>] [--mock]";

function pick(argv: string[]): { p: Provider; args: string[] } {
  const useMock = argv.includes("--mock") || process.env.HERDR_CURSOR_MOCK === "1";
  return { p: useMock ? mock() : rest, args: argv.filter((a) => a !== "--mock") };
}

async function main(): Promise<void> {
  const { p, args } = pick(process.argv.slice(2));
  const cmd = args[0] ?? "roster";
  if (cmd === "list") {
    const agents = await p.list();
    if (args.includes("--json")) {
      process.stdout.write(JSON.stringify(agents, null, 2) + "\n");
    } else {
      for (const a of agents) process.stdout.write(`${a.id}  ${a.status}  ${a.name ?? ""}\n`);
    }
  } else if (cmd === "attach") {
    if (!args[1]) throw new Error(USAGE);
    await attach(p, args[1]);
  } else if (cmd === "roster") {
    await roster(p);
  } else {
    throw new Error(`unknown command '${cmd}'\n${USAGE}`);
  }
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(`herdr-cursor: ${(e as Error).message}`);
    process.exit(1);
  },
);
