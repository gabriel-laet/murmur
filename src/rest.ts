// Cursor background-agents API over plain fetch — zero dependencies.
// Response shapes are parsed permissively (the API is young); anything
// shape-specific lives in toAgent()/parseEvent() so fixes stay local.

import type { CloudAgent, Provider, RunEvent } from "./provider.js";

const API = process.env.CURSOR_API_URL ?? "https://api.cursor.com/v1";

function key(): string {
  const k = process.env.CURSOR_API_KEY;
  if (!k) throw new Error("CURSOR_API_KEY is not set (Cursor dashboard → API keys)");
  return k;
}

async function call(method: string, path: string, body?: unknown): Promise<any> {
  const res = await fetch(`${API}${path}`, {
    method,
    headers: { "content-type": "application/json", authorization: `Bearer ${key()}` },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path} → ${res.status}: ${text.slice(0, 400)}`);
  return text ? JSON.parse(text) : {};
}

function toAgent(a: any): CloudAgent {
  return {
    id: String(a.id ?? a.agentId ?? ""),
    status: String(a.status ?? a.state ?? "UNKNOWN"),
    name: a.name ?? a.summary ?? undefined,
    branch: a.target?.branchName ?? a.branchName ?? undefined,
    prUrl: a.target?.prUrl ?? a.prUrl ?? undefined,
    createdAt: a.createdAt ?? undefined,
  };
}

function parseEvent(data: string): RunEvent {
  try {
    const v = JSON.parse(data);
    const text = v.text ?? v.message ?? v.delta ?? JSON.stringify(v);
    return { kind: v.type === "status" ? "status" : "message", text: String(text) };
  } catch {
    return { kind: "raw", text: data };
  }
}

export const rest: Provider = {
  async list() {
    const v = await call("GET", "/agents");
    // Live API wraps as {items:[...]} (verified 2026-08-19); older shapes kept.
    const arr = Array.isArray(v) ? v : (v.items ?? v.agents ?? v.data ?? []);
    return arr.map(toAgent).filter((a: CloudAgent) => a.id);
  },

  async get(id) {
    return toAgent(await call("GET", `/agents/${id}`));
  },

  async latestRunId(id) {
    const v = await call("GET", `/agents/${id}/runs`);
    const runs = Array.isArray(v) ? v : (v.items ?? v.runs ?? v.data ?? []);
    const last = runs[runs.length - 1];
    return last ? String(last.id ?? last.runId) : null;
  },

  async stream(id, runId, onEvent) {
    const res = await fetch(`${API}/agents/${id}/runs/${runId}/stream`, {
      headers: { accept: "text/event-stream", authorization: `Bearer ${key()}` },
    });
    if (!res.ok || !res.body) throw new Error(`stream ${id}/${runId} → ${res.status}`);
    const reader = res.body.getReader();
    const dec = new TextDecoder();
    let buf = "";
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i).trimEnd();
        buf = buf.slice(i + 1);
        if (!line.startsWith("data:")) continue;
        const data = line.slice(5).trim();
        if (data) onEvent(parseEvent(data));
      }
    }
    onEvent({ kind: "done", text: "" });
  },

  async followup(id, text) {
    const v = await call("POST", `/agents/${id}/runs`, { prompt: { text } });
    return { runId: String(v.id ?? v.runId ?? v.run?.id ?? "") };
  },
};
