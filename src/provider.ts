// The provider seam: everything the TUI needs from "a place cloud agents
// run", so the mock and the real API are interchangeable and the day a
// second provider (or Cursor's own CLI attach) appears, it's one new file.

export type AgentStatus = string; // CREATING | ACTIVE | RUNNING | FINISHED | ERROR | EXPIRED | ARCHIVED | ...

export interface CloudAgent {
  id: string;
  status: AgentStatus;
  name?: string;
  branch?: string;
  prUrl?: string;
  createdAt?: string;
}

export interface RunEvent {
  kind: "message" | "status" | "done" | "raw";
  text: string;
}

export interface Provider {
  list(): Promise<CloudAgent[]>;
  get(id: string): Promise<CloudAgent>;
  latestRunId(id: string): Promise<string | null>;
  /** Resolves when the run's stream ends; emits a final {kind:"done"}. */
  stream(id: string, runId: string, onEvent: (e: RunEvent) => void): Promise<void>;
  followup(id: string, text: string): Promise<{ runId: string }>;
}
