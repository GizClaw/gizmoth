import test from "node:test";
import assert from "node:assert/strict";
import { isPreviewAppMissing, previewCommandError, runPreviewApp, startPreviewFirmware, waitForPreviewCommands } from "../src/preview-app.ts";

function startupPlayer() {
  type State = Parameters<typeof startPreviewFirmware>[0]["state"];
  const state: { -readonly [K in keyof State]: State[K] } = {
    mode: "preview", phase: "idle", firmware: null, error: null,
  };
  const calls: string[] = [];
  return {
    state,
    calls,
    firmwares: [{ id: "trial" }],
    async enter() {
      calls.push("enter");
      state.mode = "play";
      state.phase = "ready";
      state.firmware = "trial";
    },
    async install(id: string) {
      calls.push(`install:${id}`);
      state.firmware = id;
      state.phase = "ready";
    },
    start() {
      calls.push("start");
      assert.equal(state.phase, "ready");
      state.phase = "starting";
      queueMicrotask(() => { state.phase = "running"; });
    },
  };
}
const startupOptions = () => ({ signal: new AbortController().signal, retryMs: 1 });

test("a Run request installs default firmware and explicitly powers it on without a Power key", async () => {
  const player = startupPlayer();
  await startPreviewFirmware(player, startupOptions());
  assert.deepEqual(player.calls, ["enter", "start"]);
  assert.equal(player.state.phase, "running");
});

test("installed firmware waiting at ready is started rather than waiting forever", async () => {
  const player = startupPlayer();
  await player.enter();
  player.calls.length = 0;
  await startPreviewFirmware(player, startupOptions());
  assert.deepEqual(player.calls, ["start"]);
});

test("an already running device is not restarted or reinstalled by another Run request", async () => {
  const player = startupPlayer();
  player.state.mode = "play";
  player.state.phase = "running";
  await startPreviewFirmware(player, startupOptions());
  assert.deepEqual(player.calls, []);
});

test("an in-flight firmware start is awaited without a second start", async () => {
  const player = startupPlayer();
  player.state.mode = "play";
  player.state.phase = "starting";
  const running = startPreviewFirmware(player, startupOptions());
  setTimeout(() => { player.state.phase = "running"; }, 5);
  await running;
  assert.deepEqual(player.calls, []);
});

test("a stopped spent runtime is reinstalled before being started", async () => {
  const player = startupPlayer();
  player.state.mode = "play";
  player.state.phase = "stopped";
  player.state.firmware = "trial";
  await startPreviewFirmware(player, startupOptions());
  assert.deepEqual(player.calls, ["install:trial", "start"]);
});

test("stopping a request during firmware installation prevents subsequent power-on", async () => {
  const controller = new AbortController();
  const player = startupPlayer();
  const install = player.enter.bind(player);
  player.enter = async () => { await install(); controller.abort(); };
  await assert.rejects(startPreviewFirmware(player, { signal: controller.signal }), { name: "AbortError" });
  assert.deepEqual(player.calls, ["enter"]);
});

for (const phase of ["idle", "stopped"] as const) {
  test(`cancel releases a pending ${phase === "idle" ? "enter" : "install"} waiter and Retry joins its installation`, async () => {
    const player = startupPlayer();
    if (phase === "stopped") {
      player.state.mode = "play";
      player.state.phase = "stopped";
      player.state.firmware = "trial";
    }
    let release!: () => void;
    const held = new Promise<void>((resolve) => { release = resolve; });
    const install = phase === "idle" ? player.enter.bind(player) : () => player.install("trial");
    const operation = async () => { await held; await install(); };
    if (phase === "idle") player.enter = operation;
    else {
      const original = player.install.bind(player);
      player.install = async (id) => { await held; await original(id); };
    }
    const controller = new AbortController();
    const first = startPreviewFirmware(player, { signal: controller.signal });
    await Promise.resolve();
    controller.abort();
    await assert.rejects(first, { name: "AbortError" });
    assert.deepEqual(player.calls, [], "cancellation does not wait for SDK settlement or boot");
    const retried = startPreviewFirmware(player, startupOptions());
    await Promise.resolve();
    assert.deepEqual(player.calls, []);
    release();
    await retried;
    assert.deepEqual(player.calls, [phase === "idle" ? "enter" : "install:trial", "start"]);
  });
}

test("firmware installation has a deadline before its SDK operation settles", async () => {
  const player = startupPlayer();
  let rejectInstall!: (error: Error) => void;
  player.enter = () => new Promise<void>((_, reject) => { rejectInstall = reject; });
  await assert.rejects(startPreviewFirmware(player, { ...startupOptions(), timeoutMs: 10 }), /启动超时/);
  assert.deepEqual(player.calls, []);
  // Late SDK errors remain observed after the waiter has timed out.
  rejectInstall(new Error("late installation failure"));
  await Promise.resolve();
});

test("a firmware startup that never becomes ready fails without replaying start", async () => {
  const player = startupPlayer();
  await player.enter();
  player.calls.length = 0;
  player.start = () => { player.calls.push("start"); player.state.phase = "starting"; };
  await assert.rejects(startPreviewFirmware(player, { ...startupOptions(), timeoutMs: 10 }), /启动超时/);
  assert.deepEqual(player.calls, ["start"]);
});

const palError = (code: number) => Object.assign(new Error("device operation failed"), { code });
const interfaceNotReady = () =>
  new Error("Firmware GizClaw interface is not ready; use an H106 Trial WASM with Module.h2GizClaw");

test("player running waits for the command ABI and confirms Lua tools before installing", async () => {
  let calls = 0;
  await waitForPreviewCommands(
    {
      async executeCommand(command) {
        assert.deepEqual(command, { method: "tools.list" });
        if (++calls < 3) throw interfaceNotReady();
        return [{ name: "lua.app.install" }, { name: "lua.app.run" }];
      },
    },
    { signal: new AbortController().signal, retryMs: 1 },
  );
  assert.equal(calls, 3);
});

test("a firmware without Lua tools fails capability detection without repeated writes", async () => {
  let calls = 0;
  await assert.rejects(
    waitForPreviewCommands(
      {
        async executeCommand() {
          calls++;
          return [{ name: "lua.app.list" }];
        },
      },
      { signal: new AbortController().signal },
    ),
    /不支持/,
  );
  assert.equal(calls, 1);
});

test("an interface that never registers fails with a bounded startup diagnostic", async () => {
  await assert.rejects(
    waitForPreviewCommands(
      {
        async executeCommand() {
          throw interfaceNotReady();
        },
      },
      { signal: new AbortController().signal, timeoutMs: 10, retryMs: 1 },
    ),
    /命令接口未就绪/,
  );
});

test("canceling startup prevents commands from reaching the firmware", async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    waitForPreviewCommands(
      {
        async executeCommand() {
          assert.fail("Canceled startup must not query tools");
        },
      },
      { signal: controller.signal },
    ),
    { name: "AbortError" },
  );
});

test("a BUSY run waits for Home and retries only the launch command", async () => {
  let calls = 0;
  let waits = 0;
  await runPreviewApp(
    {
      async executeCommand(command, options) {
        assert.deepEqual(command, { method: "tools.invoke", tool: "lua.app.run", args: { app_id: "probe" } });
        assert.equal(options?.signal, undefined, "do not cancel an already submitted firmware handoff");
        if (++calls < 3) throw palError(-18);
        return {};
      },
    },
    "probe",
    { signal: new AbortController().signal, onWaiting: () => waits++, retryMs: 1 },
  );
  assert.equal(calls, 3);
  assert.equal(waits, 1);
});

test("a command interface rebind while waiting does not discard an installed app", async () => {
  let calls = 0;
  await runPreviewApp(
    {
      async executeCommand(command) {
        assert.equal(command.method, "tools.invoke");
        if (++calls === 1) throw palError(-18);
        if (calls === 2) throw interfaceNotReady();
        return {};
      },
    },
    "probe",
    { signal: new AbortController().signal, onWaiting: () => {}, retryMs: 1 },
  );
  assert.equal(calls, 3);
});

test("the real firmware RPC resource-exhausted envelope retries a rejected launch", async () => {
  const error = Object.assign(new Error("device operation failed"), { code: -1000, rpcCode: 8 });
  let calls = 0;
  await runPreviewApp(
    {
      async executeCommand() {
        if (++calls === 1) throw error;
        return {};
      },
    },
    "probe",
    { signal: new AbortController().signal, onWaiting: () => {}, retryMs: 1 },
  );
  assert.equal(calls, 2);
  assert.match(previewCommandError(error)!, /设备仍忙/);
});

for (const [source, error] of [
  ["PAL", palError(-8)],
  ["RPC", Object.assign(new Error("device operation failed"), { code: -1000, rpcCode: 5 })],
] as const) {
  test(`a definitive ${source} missing-app error is classified without replaying the launch`, async () => {
    assert.equal(isPreviewAppMissing(error), true);
    assert.match(previewCommandError(error)!, /重新安装后重试/);
    let calls = 0;
    await assert.rejects(
      runPreviewApp(
        {
          async executeCommand() {
            calls++;
            throw error;
          },
        },
        "probe",
        { signal: new AbortController().signal, onWaiting: () => assert.fail("Missing app must not retry") },
      ),
      (result) => result === error,
    );
    assert.equal(calls, 1);
  });
}

test("ambiguous failures, BUSY and interface rebinds do not invalidate an acknowledged install", () => {
  for (const error of [
    palError(-6),
    palError(-18),
    palError(-3),
    palError(-7),
    ...[4, 8, 12].map((rpcCode) => Object.assign(new Error("device operation failed"), { code: -1000, rpcCode })),
    interfaceNotReady(),
    new Error("device operation failed"),
    Object.assign(new Error("device operation failed"), { rpcCode: 5 }),
    Object.assign(new Error("device operation failed"), { code: -18, rpcCode: 5 }),
    { code: "-8" },
    { code: -1000, rpcCode: "5" },
    null,
  ]) {
    assert.equal(isPreviewAppMissing(error), false);
  }
});

test("an RPC deadline error is not mistaken for a rejected launch", async () => {
  const error = Object.assign(new Error("device operation failed"), { code: -1000, rpcCode: 4 });
  let calls = 0;
  await assert.rejects(
    runPreviewApp(
      {
        async executeCommand() {
          calls++;
          throw error;
        },
      },
      "probe",
      { signal: new AbortController().signal, onWaiting: () => assert.fail("Ambiguous run must not retry") },
    ),
    (value) => value === error,
  );
  assert.equal(calls, 1);
  assert.match(previewCommandError(error)!, /结果尚未确认/);
});

for (const code of [-6, -3, -8, -7]) {
  test(`a non-BUSY error (${code}) never replays a possibly accepted launch`, async () => {
    let calls = 0;
    const error = palError(code);
    await assert.rejects(
      runPreviewApp(
        {
          async executeCommand() {
            calls++;
            throw error;
          },
        },
        "probe",
        { signal: new AbortController().signal, onWaiting: () => assert.fail("Must not wait") },
      ),
      (result) => result === error,
    );
    assert.equal(calls, 1);
  });
}

test("a replaced or canceled app stops waiting before another command", async () => {
  const controller = new AbortController();
  let calls = 0;
  await assert.rejects(
    runPreviewApp(
      {
        async executeCommand() {
          calls++;
          throw palError(-18);
        },
      },
      "old-app",
      { signal: controller.signal, onWaiting: () => controller.abort() },
    ),
    { name: "AbortError" },
  );
  assert.equal(calls, 1);
});

test("canceling during the retry delay releases the waiter immediately", async () => {
  const controller = new AbortController();
  let calls = 0;
  const run = runPreviewApp(
    {
      async executeCommand() {
        calls++;
        throw palError(-18);
      },
    },
    "probe",
    { signal: controller.signal, onWaiting: () => setTimeout(() => controller.abort(), 1), retryMs: 10000 },
  );
  await assert.rejects(run, { name: "AbortError" });
  assert.equal(calls, 1);
});

test("waiting has a deadline and reports device state rather than incompatibility", async () => {
  await assert.rejects(
    runPreviewApp(
      {
        async executeCommand() {
          throw palError(-18);
        },
      },
      "probe",
      { signal: new AbortController().signal, onWaiting: () => {}, timeoutMs: 10, retryMs: 1 },
    ),
    /设备仍忙.*首次开机引导/,
  );
});

test("firmware diagnostics keep BUSY, unsupported and timeout distinct", () => {
  assert.match(previewCommandError(palError(-18))!, /首次开机引导/);
  assert.match(previewCommandError(palError(-3))!, /不支持/);
  assert.match(previewCommandError(palError(-6))!, /结果尚未确认/);
  assert.equal(previewCommandError(new Error("other failure")), null);
});
