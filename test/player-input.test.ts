import test, { type TestContext } from "node:test";
import assert from "node:assert/strict";
import { createPreviewInput } from "../src/preview-input.ts";
import { createSimulatorController, type SimulatorSnapshot } from "../src/simulator-control.ts";

async function buttonsFixture(t: TestContext) {
  const owner = Object.assign(new EventTarget(), { parent: null as unknown });
  owner.parent = owner;
  const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: owner });
  const { createH106ModelButtons } = await import(
    new URL("../node_modules/@gizclaw/models-player/dist/buttons.js", import.meta.url).href
  );
  const buttons = createH106ModelButtons("tiga");
  t.after(() => {
    buttons.destroy();
    if (previous) Object.defineProperty(globalThis, "window", previous);
    else Reflect.deleteProperty(globalThis, "window");
  });
  return { owner, buttons };
}

test("SDK focus cleanup releases user keys but preserves programmatic holds", async (t) => {
  const { owner, buttons } = await buttonsFixture(t);
  buttons.press("back", "agent", { releaseOnBlur: false });
  buttons.press("left", "keyboard");
  owner.dispatchEvent(new Event("blur"));
  assert.deepEqual(buttons.getPressed(), ["back"]);
  buttons.press("ok", "pointer");
  buttons.releaseUserInput();
  assert.deepEqual(buttons.getPressed(), ["back"]);
  buttons.release("back", "agent");
  assert.deepEqual(buttons.getPressed(), []);
});

test("SDK shutdown, pagehide and destruction release programmatic holds", async (t) => {
  const { owner, buttons } = await buttonsFixture(t);
  buttons.press("back", "agent", { releaseOnBlur: false });
  buttons.releaseAll();
  assert.deepEqual(buttons.getPressed(), []);
  buttons.press("back", "agent", { releaseOnBlur: false });
  owner.dispatchEvent(new Event("pagehide"));
  assert.deepEqual(buttons.getPressed(), []);
  buttons.press("back", "agent", { releaseOnBlur: false });
  buttons.destroy();
  assert.deepEqual(buttons.getPressed(), []);
});

test("canceling an agent hold after blur still releases its real SDK source", async (t) => {
  const { owner, buttons } = await buttonsFixture(t);
  const input = createPreviewInput({
    press: (button, source) => buttons.press(button, source, { releaseOnBlur: false }),
    release: (button, source) => buttons.release(button, source),
  });
  const snapshot: SimulatorSnapshot = {
    profileId: "sim",
    requestId: "app",
    phase: "running",
    firmware: "trial",
    app: { appId: "demo", status: "run-requested" },
    error: null,
    notice: null,
    events: [],
  };
  const controller = createSimulatorController({
    snapshot: () => snapshot,
    start: async () => {},
    press: input.press,
    release: input.release,
    capture: () => {
      throw new Error("not used");
    },
  });
  const abort = new AbortController();
  const held = controller({ action: "press", requestId: "app", button: "back", durationMs: 3000 }, abort.signal);
  owner.dispatchEvent(new Event("blur"));
  buttons.releaseUserInput();
  assert.deepEqual(buttons.getPressed(), ["back"]);
  abort.abort();
  await assert.rejects(held, { name: "AbortError" });
  assert.deepEqual(buttons.getPressed(), []);
});
