import test from "node:test";
import assert from "node:assert/strict";
import { createSimulatorController, type SimulatorSnapshot } from "../src/simulator-control.ts";

function fixture() {
  const state: SimulatorSnapshot = {
    profileId: "sim",
    requestId: "app-1",
    phase: "running",
    firmware: "trial",
    app: { appId: "demo", status: "run-requested" },
    error: null,
    notice: null,
    events: [],
  };
  const edges: string[] = [];
  let starts = 0;
  const controller = createSimulatorController({
    snapshot: () => ({ ...state }),
    start: async () => {
      starts++;
    },
    press: (button, source) => {
      edges.push(`press:${button}:${source}`);
    },
    release: (source) => {
      edges.push(`release:${source}`);
    },
    capture: () => ({ data: "png", mimeType: "image/png", width: 240, height: 240, brightness: 0, hash: "00000000" }),
  });
  return { state, edges, controller, starts: () => starts };
}
const signal = () => new AbortController().signal;

test("simulator inspection preserves run admission and sleeping screen rather than inventing a pass", async () => {
  const f = fixture();
  const result = await f.controller({ action: "inspect", requestId: "app-1", capture: true }, signal());
  assert.equal(result.app.status, "run-requested");
  assert.equal(result.screen?.brightness, 0);
  assert.equal(result.screen?.data, "png");
  assert.equal(f.starts(), 0);
});

test("cancellation always releases a held simulator button", async () => {
  const f = fixture();
  const abort = new AbortController();
  const pending = f.controller({ action: "press", requestId: "app-1", button: "back", durationMs: 3000 }, abort.signal);
  assert.equal(f.edges.length, 1);
  abort.abort();
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(f.edges.length, 2);
  assert.equal(f.edges[0]?.slice("press:back:".length), f.edges[1]?.slice("release:".length));
});

test("invalid input and stale project targets cannot start firmware or emit buttons", async () => {
  const f = fixture();
  for (const command of [
    { action: "start", requestId: "other-project" },
    { action: "press", requestId: "app-1", button: "up" },
    { action: "press", requestId: "app-1", button: "ok", durationMs: 3001 },
    { action: "press", requestId: "app-1", button: "ok", durationMs: Number.NaN },
    { action: "inspect", requestId: "app-1", waitMs: 10001 },
  ]) {
    await assert.rejects(f.controller(command as Parameters<typeof f.controller>[0], signal()));
  }
  assert.equal(f.edges.length, 0);
  assert.equal(f.starts(), 0);
});

test("retargeting while a button is held releases it and refuses a result from another app", async () => {
  const f = fixture();
  const pending = f.controller({ action: "press", requestId: "app-1", button: "ok", durationMs: 1 }, signal());
  f.state.requestId = "app-2";
  await assert.rejects(pending, /target changed/);
  assert.equal(f.edges.length, 2);
});

test("concurrent button commands cannot interleave their physical edges", async () => {
  const f = fixture();
  const first = f.controller({ action: "press", requestId: "app-1", button: "ok", durationMs: 1 }, signal());
  await assert.rejects(
    f.controller({ action: "press", requestId: "app-1", button: "back" }, signal()),
    /already running/,
  );
  await first;
  assert.equal(f.edges.length, 2);
});
