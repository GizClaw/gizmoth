// Exercise the actual patched SDK in a real DOM without paint callbacks, WebGL,
// firmware downloads or cloud services. This proves lifecycle behavior only;
// the opt-in native suite separately proves real Lua execution and storage.
import { mountModelPlay } from "@gizclaw/models-player/play";

const result = document.querySelector("#result");
const host = document.querySelector("#host");
const original = {
  requestAnimationFrame: window.requestAnimationFrame,
  cancelAnimationFrame: window.cancelAnimationFrame,
  setTimeout: window.setTimeout,
  clearTimeout: window.clearTimeout,
};
const pending = new Set();
let paintRequests = 0;
window.requestAnimationFrame = () => ++paintRequests;
window.cancelAnimationFrame = () => {};
window.setTimeout = (callback, ms, ...args) => {
  const id = original.setTimeout.call(
    window,
    () => {
      pending.delete(id);
      callback(...args);
    },
    ms,
  );
  pending.add(id);
  return id;
};
window.clearTimeout = (id) => {
  pending.delete(id);
  original.clearTimeout.call(window, id);
};
const pause = (ms) => new Promise((resolve) => original.setTimeout.call(window, resolve, ms));
const assert = (condition, message) => {
  if (!condition) throw new Error(message);
};
const players = [];
async function fixture() {
  const firmware = { id: "lifecycle-probe", name: "Lifecycle probe", model: "h106-tiga" };
  const runtime = {
    started: false,
    ready: false,
    canvas: document.createElement("canvas"),
    start() {
      this.started = true;
    },
    shutdown() {
      this.started = false;
    },
    destroy() {},
    button() {},
    setEnvironment() {},
    executeCommand: async () => "accepted",
  };
  const states = [];
  const player = mountModelPlay(
    host,
    {
      model: "h106-tiga",
      presentation: "inline",
      shortcuts: false,
      modelsProvider: { listFirmwares: async () => [firmware] },
      resolveFirmware: async () => ({}),
      onStateChange: (state) => states.push(state.phase),
    },
    {
      loadModelPlayer: async () => ({
        createH106Viewer: (stage) => {
          stage.append(document.createElement("canvas"));
          return {
            ready: Promise.resolve(),
            requestRender() {},
            setRenderingEnabled() {},
            setInteractiveCore() {},
            closeSimGuide() {},
            buttons: { releaseAll() {}, releaseUserInput() {}, subscribe: () => () => {} },
            destroy() {},
          };
        },
      }),
      createFirmware: async () => runtime,
    },
  );
  players.push(player);
  await player.ready;
  await player.enter();
  assert(player.state.phase === "ready", "Fixture must reach the real SDK ready phase");
  return { player, runtime, states };
}
try {
  const running = await fixture();
  running.player.start();
  assert(running.player.state.phase === "starting", "Startup must wait for runtime readiness");
  running.runtime.ready = true;
  await pause(100);
  assert(running.player.state.phase === "running", "Readiness must advance without any paint callback");
  assert(
    (await running.player.executeCommand({ method: "tools.list" })) === "accepted",
    "Commands must become available",
  );
  assert(pending.size === 0, "Readiness polling must stop after reaching running");
  await running.player.shutdown();
  running.player.destroy();

  const stopping = await fixture();
  stopping.player.start();
  let release;
  stopping.runtime.shutdown = () =>
    new Promise((resolve) => {
      release = resolve;
    });
  const shutdown = stopping.player.shutdown();
  stopping.runtime.ready = true;
  await pause(75);
  assert(stopping.player.state.phase === "stopped", "Pending shutdown must never return to running");
  assert(!stopping.states.includes("running"), "No late running event may escape shutdown");
  assert(pending.size === 0, "Shutdown must cancel the startup timer immediately");
  release();
  await shutdown;
  stopping.player.destroy();

  const destroyed = await fixture();
  destroyed.player.start();
  destroyed.player.destroy();
  assert(pending.size === 0, "Destroy must cancel the startup timer immediately");
  destroyed.runtime.ready = true;
  await pause(75);
  assert(destroyed.player.state.phase === "destroyed", "Destroy must remain terminal");
  assert(!destroyed.states.includes("running"), "Destroyed players must never announce running");
  assert(paintRequests === 0, "Lifecycle progress must not request paint frames");
  result.textContent = "PASS: readiness, command admission, shutdown and destroy work without paint callbacks.";
  result.dataset.result = "pass";
} catch (error) {
  result.textContent = `FAIL: ${error.message}`;
  result.dataset.result = "fail";
  console.error(error);
} finally {
  for (const player of players) player.destroy();
  for (const id of pending) original.clearTimeout.call(window, id);
  Object.assign(window, original);
}
