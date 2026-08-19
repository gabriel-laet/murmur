// The interactive TUI, on Ink (React for terminals). Two apps: the roster
// (all cloud agents, live) and the attach view (one agent: stream + follow-
// ups). Non-TTY callers never reach this file — roster.ts/attach.ts keep
// plain output for pipes.

import React, { useEffect, useRef, useState } from "react";
import { render, Box, Text, Static, useApp, useInput } from "ink";
import type { CloudAgent, Provider, RunEvent } from "./provider.js";
import { inside, openAttachPane, reportState, statusToHerdrState } from "./herdr.js";

function eventColor(kind: RunEvent["kind"]): string | undefined {
  if (kind === "status") return "cyan";
  if (kind === "tool") return "yellow";
  return undefined;
}

function EventLine({ e }: { e: RunEvent }) {
  const dim = e.kind === "thinking" || e.kind === "usage";
  const prefix = e.kind === "status" ? "· " : "";
  return (
    <Text color={eventColor(e.kind)} dimColor={dim}>
      {prefix}
      {e.text}
    </Text>
  );
}

// ---- attach: one agent, streamed, steerable ----

function AttachApp({ p, id }: { p: Provider; id: string }) {
  const { exit } = useApp();
  const [agent, setAgent] = useState<CloudAgent | null>(null);
  const [lines, setLines] = useState<RunEvent[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const seq = useRef(0);
  const append = (e: RunEvent) => setLines((xs) => [...xs, e]);

  const streamRun = async (runId: string) => {
    setBusy(true);
    setErr(null);
    reportState("working");
    try {
      await p.stream(id, runId, (e) => {
        if (e.kind !== "done") append(e);
      });
      append({ kind: "status", text: "run finished" });
    } catch (e) {
      setErr((e as Error).message);
    }
    reportState("idle");
    setBusy(false);
  };

  useEffect(() => {
    void (async () => {
      try {
        const a = await p.get(id);
        setAgent(a);
        reportState(statusToHerdrState(a.status));
        const runId = await p.latestRunId(id);
        if (runId) await streamRun(runId);
      } catch (e) {
        setErr((e as Error).message);
      }
    })();
  }, []);

  useInput((ch, key) => {
    if (key.escape || (key.ctrl && ch === "c")) {
      reportState("idle");
      exit();
      return;
    }
    if (key.return) {
      const text = input.trim();
      setInput("");
      if (!text) return;
      if (text === "/q") {
        reportState("idle");
        exit();
        return;
      }
      if (busy) {
        setErr("a run is still streaming — one active run per agent");
        return;
      }
      void (async () => {
        try {
          append({ kind: "status", text: `you: ${text}` });
          const { runId } = await p.followup(id, text);
          await streamRun(runId);
        } catch (e) {
          setErr((e as Error).message); // 409 agent_busy lands here
        }
      })();
      return;
    }
    if (key.backspace || key.delete) setInput((s) => s.slice(0, -1));
    else if (ch && !key.ctrl && !key.meta) setInput((s) => s + ch);
  });

  return (
    <Box flexDirection="column">
      <Static items={lines.map((e) => ({ e, k: seq.current++ }))}>
        {({ e, k }) => <EventLine key={k} e={e} />}
      </Static>
      {err && <Text color="red">error: {err}</Text>}
      <Box>
        <Text color={busy ? "yellow" : "green"}>{busy ? "streaming " : ""}follow-up&gt; </Text>
        <Text>{input}</Text>
      </Box>
      <Text dimColor>
        {agent ? `${agent.id}  ${agent.status}${agent.prUrl ? `  ${agent.prUrl}` : ""}  — ` : ""}
        enter send · /q or esc quit
      </Text>
    </Box>
  );
}

// ---- roster: every cloud agent, live ----

function RosterApp({
  p,
  onInline,
}: {
  p: Provider;
  onInline: (id: string) => void;
}) {
  const { exit } = useApp();
  const [agents, setAgents] = useState<CloudAgent[]>([]);
  const [sel, setSel] = useState(0);
  const [note, setNote] = useState(
    "↑/↓ select · enter attach · a panes for all ACTIVE · r refresh · q quit",
  );

  const refresh = async () => {
    try {
      const a = await p.list();
      setAgents(a);
      setSel((s) => Math.min(s, Math.max(0, a.length - 1)));
    } catch (e) {
      setNote(`error: ${(e as Error).message}`);
    }
  };

  useEffect(() => {
    void refresh();
    const t = setInterval(() => void refresh(), Number(process.env.HERDR_CURSOR_POLL_MS ?? 5000));
    return () => clearInterval(t);
  }, []);

  useInput((ch, key) => {
    if (ch === "q" || (key.ctrl && ch === "c")) return exit();
    if (key.upArrow || ch === "k") setSel((s) => Math.max(0, s - 1));
    if (key.downArrow || ch === "j") setSel((s) => Math.min(agents.length - 1, s + 1));
    if (ch === "r") return void refresh();
    if (key.return && agents[sel]) {
      if (inside()) {
        openAttachPane(agents[sel].id);
        setNote(`opened a pane for ${agents[sel].id}`);
      } else {
        onInline(agents[sel].id);
        exit();
      }
    }
    if (ch === "a") {
      const active = agents.filter((x) => {
        const s = x.status.toUpperCase();
        return s === "ACTIVE" || s === "RUNNING";
      });
      if (!inside()) setNote("not inside herdr — no panes to open");
      else if (active.length === 0) setNote("no ACTIVE agents");
      else {
        active.forEach((x) => openAttachPane(x.id));
        setNote(`opened ${active.length} pane(s)`);
      }
    }
  });

  const working = (s: string) => {
    const u = s.toUpperCase();
    return u === "ACTIVE" || u === "RUNNING" || u === "CREATING";
  };

  return (
    <Box flexDirection="column">
      <Text bold>cursor cloud agents ({agents.length})</Text>
      <Box height={1} />
      {agents.map((a, i) => (
        <Text key={a.id} inverse={i === sel}>
          {i === sel ? "▸ " : "  "}
          <Text color={working(a.status) ? "green" : "gray"}>{a.status.padEnd(9)}</Text>{" "}
          {a.id.padEnd(14)} {a.name ?? ""}
          {a.prUrl ? <Text dimColor>  ({a.prUrl})</Text> : null}
        </Text>
      ))}
      <Box height={1} />
      <Text dimColor>{note}</Text>
    </Box>
  );
}

// ---- entry points ----

export async function runAttach(p: Provider, id: string): Promise<void> {
  const app = render(<AttachApp p={p} id={id} />);
  await app.waitUntilExit();
}

export async function runRoster(p: Provider): Promise<void> {
  let inlineId: string | null = null;
  const app = render(<RosterApp p={p} onInline={(id) => (inlineId = id)} />);
  await app.waitUntilExit();
  if (inlineId) await runAttach(p, inlineId);
}
