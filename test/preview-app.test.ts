import test from "node:test";
import assert from "node:assert/strict";
import { previewCommandError, runPreviewApp, waitForPreviewCommands } from "../src/preview-app.ts";

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
