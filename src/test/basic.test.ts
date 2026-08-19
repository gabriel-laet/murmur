import test from "node:test";
import assert from "node:assert/strict";
import { mock } from "../mock.js";
import { statusToHerdrState } from "../herdr.js";

test("mock lists agents and streams a run to completion", async () => {
  const p = mock(1);
  const agents = await p.list();
  assert.ok(agents.length >= 2);
  const runId = await p.latestRunId(agents[0].id);
  assert.ok(runId);
  const kinds: string[] = [];
  await p.stream(agents[0].id, runId!, (e) => kinds.push(e.kind));
  assert.ok(kinds.length > 1);
  assert.equal(kinds[kinds.length - 1], "done");
});

test("followup returns a fresh run id each time", async () => {
  const p = mock(1);
  const a = await p.followup("bc-mock-1", "keep going");
  const b = await p.followup("bc-mock-1", "more");
  assert.notEqual(a.runId, b.runId);
});

test("unknown agents fail loudly", async () => {
  const p = mock(1);
  await assert.rejects(() => p.get("bc-nope"), /no such agent/);
});

test("cloud statuses map onto herdr pane states", () => {
  assert.equal(statusToHerdrState("ACTIVE"), "working");
  assert.equal(statusToHerdrState("CREATING"), "working");
  assert.equal(statusToHerdrState("RUNNING"), "working");
  assert.equal(statusToHerdrState("FINISHED"), "idle");
  assert.equal(statusToHerdrState("ARCHIVED"), "idle");
  assert.equal(statusToHerdrState("ERROR"), "idle");
});

test("sdk messages map to display events", async () => {
  const { sdkMessageToEvent } = await import("../sdk.js");
  const base = { agent_id: "bc-1", run_id: "r-1" };
  assert.deepEqual(
    sdkMessageToEvent({ type: "tool_call", ...base, call_id: "c1", name: "edit", status: "running" } as any),
    { kind: "tool", text: "▸ edit" },
  );
  assert.equal(
    sdkMessageToEvent({ type: "tool_call", ...base, call_id: "c1", name: "edit", status: "completed" } as any),
    null,
  );
  assert.deepEqual(
    sdkMessageToEvent({
      type: "assistant", ...base,
      message: { role: "assistant", content: [{ type: "text", text: "done." }] },
    } as any),
    { kind: "message", text: "done." },
  );
  assert.deepEqual(
    sdkMessageToEvent({ type: "status", ...base, status: "RUNNING", message: "cloning" } as any),
    { kind: "status", text: "RUNNING — cloning" },
  );
  assert.equal(sdkMessageToEvent({ type: "system", subtype: "init", ...base } as any), null);
});
