// The default provider: Cursor's official SDK. Richer than rest.ts in
// every dimension that matters for a live pane — Agent.resume() attaches
// to any bc- id (including agents murmur launched over REST), and
// run.stream() yields typed events (assistant, tool_call, thinking,
// status, usage) instead of SSE lines we'd have to guess at.

import { Agent } from "@cursor/sdk";
import type { Run, SDKAgentInfo, SDKMessage } from "@cursor/sdk";
import type { CloudAgent, Provider, RunEvent } from "./provider.js";

function toAgent(info: SDKAgentInfo): CloudAgent {
  const status = info.archived ? "ARCHIVED" : (info.status ?? "unknown").toUpperCase();
  return {
    id: info.agentId,
    status,
    name: info.name || info.summary || undefined,
    createdAt: info.createdAt ? new Date(info.createdAt).toISOString() : undefined,
  };
}

/** Pure SDKMessage → display-line mapping; null = not worth a line. */
export function sdkMessageToEvent(m: SDKMessage): RunEvent | null {
  switch (m.type) {
    case "assistant": {
      const text = m.message.content
        .filter((b): b is { type: "text"; text: string } => b.type === "text")
        .map((b) => b.text)
        .join("");
      return text.trim() ? { kind: "message", text } : null;
    }
    case "tool_call":
      if (m.status === "running") return { kind: "tool", text: `▸ ${m.name}` };
      if (m.status === "error") return { kind: "tool", text: `✗ ${m.name}` };
      return null; // completed: the ▸ line already told the story
    case "thinking": {
      const text = m.text.trim();
      if (!text) return null;
      return { kind: "thinking", text: text.length > 200 ? `${text.slice(0, 200)}…` : text };
    }
    case "status":
      return { kind: "status", text: m.message ? `${m.status} — ${m.message}` : m.status };
    case "task":
      return m.text ? { kind: "status", text: m.text } : null;
    case "usage": {
      const parts = Object.entries(m.usage as unknown as Record<string, unknown>)
        .filter(([, v]) => typeof v === "number")
        .map(([k, v]) => `${k} ${v}`);
      return parts.length ? { kind: "usage", text: `tokens: ${parts.join(", ")}` } : null;
    }
    default:
      return null; // system init, user echo, request
  }
}

async function latestRun(id: string): Promise<Run | null> {
  const res = await Agent.listRuns(id, { runtime: "cloud" });
  if (res.items.length === 0) return null;
  return res.items.reduce((a, b) => ((b.createdAt ?? 0) > (a.createdAt ?? 0) ? b : a));
}

async function streamRun(run: Run, onEvent: (e: RunEvent) => void): Promise<void> {
  if (run.supports("stream")) {
    for await (const m of run.stream()) {
      const e = sdkMessageToEvent(m);
      if (e) onEvent(e);
    }
  } else if (run.supports("wait")) {
    // Older/terminal runs may not replay a stream — settle for the result.
    const r = await run.wait();
    onEvent({ kind: "status", text: r.status });
    if (r.result) onEvent({ kind: "message", text: r.result });
    const pr = r.git?.branches.find((b) => b.prUrl)?.prUrl;
    if (pr) onEvent({ kind: "status", text: `pr ${pr}` });
  } else {
    onEvent({ kind: "status", text: `run ${run.id}: ${run.status}` });
  }
  onEvent({ kind: "done", text: "" });
}

export const sdk: Provider = {
  async list() {
    const res = await Agent.list({ runtime: "cloud" });
    return res.items.map(toAgent);
  },

  async get(id) {
    return toAgent(await Agent.get(id));
  },

  async latestRunId(id) {
    const run = await latestRun(id);
    return run ? run.id : null;
  },

  async stream(id, runId, onEvent) {
    const run = await Agent.getRun(runId, { runtime: "cloud", agentId: id });
    await streamRun(run, onEvent);
  },

  async followup(id, text) {
    const agent = await Agent.resume(id);
    const run = await agent.send(text);
    return { runId: run.id };
  },
};
