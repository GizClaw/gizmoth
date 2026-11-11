import test from "node:test";
import assert from "node:assert/strict";
import type { DevEnvState, RemovalPreview } from "../src/devenv.ts";

test("demo registry mirrors the published state: only Bazelisk is installable", async () => {
  const { demoDevEnv } = await import(new URL("../src/demo-devenv.ts?test=published", import.meta.url).href);
  const state = demoDevEnv("devenv_state", {}) as DevEnvState;
  assert.deepEqual(
    state.chips.map((chip) => [chip.id, chip.status]),
    [
      ["esp32s3", "planned"],
      ["bk7258", "planned"],
      ["ac707n", "planned"],
    ],
  );
  assert.deepEqual(
    state.toolchains.filter((item) => item.status === "available").map((item) => item.id),
    ["bazelisk"],
  );
  assert.throws(() => demoDevEnv("devenv_install", { target: { kind: "chip", id: "esp32s3" } }));
});

test("demo installation runs as a job, then removal and local registration use local records", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 1_000 });
  const { demoDevEnv } = await import(new URL("../src/demo-devenv.ts?test=lifecycle", import.meta.url).href);
  const started = demoDevEnv("devenv_install", { target: { kind: "toolchain", id: "bazelisk" } }) as DevEnvState;
  assert.equal(started.job?.state, "running");
  assert.equal(started.toolchains[0].status, "installing");
  assert.throws(() => demoDevEnv("devenv_install", { target: { kind: "toolchain", id: "bazelisk" } }), /正在进行/);

  t.mock.timers.tick(5_000);
  const done = demoDevEnv("devenv_state", {}) as DevEnvState;
  assert.equal(done.job?.state, "succeeded");
  assert.equal(done.job?.received, done.job?.total);
  assert.equal(done.toolchains[0].status, "installed");
  const [installed] = done.installations;
  assert.equal(installed.origin, "managed");

  const target = { kind: "installation", id: installed.installationId };
  const preview = demoDevEnv("devenv_remove_preview", { target }) as RemovalPreview;
  assert.deepEqual([preview.action, preview.confirm], ["delete", [installed.installationId]]);
  assert.equal(
    (demoDevEnv("devenv_remove", { target, confirm: preview.confirm }) as DevEnvState).installations.length,
    0,
  );

  const added = demoDevEnv("devenv_add_local", { toolchain: "jieli", version: "1.0.0" }) as DevEnvState;
  const local = added.installations[0];
  const unregister = demoDevEnv("devenv_remove_preview", {
    target: { kind: "installation", id: local.installationId },
  }) as RemovalPreview;
  assert.deepEqual([unregister.action, unregister.confirm], ["unregister", []]);
});

test("reinstall keeps the old payload on cancellation and replaces its directory only on success", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 1_000 });
  const { demoDevEnv } = await import(new URL("../src/demo-devenv.ts?test=reinstall", import.meta.url).href);
  demoDevEnv("devenv_install", { target: { kind: "toolchain", id: "bazelisk" } });
  t.mock.timers.tick(5_000);
  const installed = demoDevEnv("devenv_state", {}) as DevEnvState;
  const before = installed.installations[0];
  const target = { kind: "installation", id: before.installationId };
  assert.ok(before.path.endsWith(before.installationId));
  assert.equal(before.reinstallable, true);
  assert.equal(demoDevEnv("devenv_open_folder", { installationId: before.installationId }), undefined);
  assert.throws(() => demoDevEnv("devenv_open_folder", { installationId: "unknown" }), /未知安装/);

  const repairing = demoDevEnv("devenv_reinstall", { target }) as DevEnvState;
  assert.equal(repairing.job?.state, "running");
  assert.deepEqual(repairing.installations, [before]);
  assert.throws(() => demoDevEnv("devenv_remove", { target, confirm: [before.installationId] }), /正在进行/);
  const cancelled = demoDevEnv("devenv_cancel", { jobId: repairing.job!.id }) as DevEnvState;
  assert.deepEqual(cancelled.installations, [before]);

  demoDevEnv("devenv_reinstall", { target });
  t.mock.timers.tick(5_000);
  const complete = demoDevEnv("devenv_state", {}) as DevEnvState;
  assert.equal(complete.job?.state, "succeeded");
  assert.equal(complete.installations.length, 1);
  const after = complete.installations[0];
  assert.equal(after.version, before.version);
  assert.notEqual(after.installationId, before.installationId);
  assert.notEqual(after.path, before.path);
  assert.ok(after.path.endsWith(after.installationId));
  assert.throws(() => demoDevEnv("devenv_open_folder", { installationId: before.installationId }), /未知安装/);
  const remove = { kind: "installation", id: after.installationId };
  assert.throws(() => demoDevEnv("devenv_remove", { target: remove, confirm: [] }), /重新确认/);
  assert.equal(
    (demoDevEnv("devenv_remove", { target: remove, confirm: [after.installationId] }) as DevEnvState).installations
      .length,
    0,
  );
});

test("registered local toolchains can be opened and unregistered but never reinstalled", async () => {
  const { demoDevEnv } = await import(new URL("../src/demo-devenv.ts?test=local-reinstall", import.meta.url).href);
  const state = demoDevEnv("devenv_add_local", { toolchain: "arm-gnu", version: "10.3.0" }) as DevEnvState;
  const local = state.installations[0];
  const target = { kind: "installation", id: local.installationId };
  assert.equal(local.reinstallable, false);
  assert.equal(demoDevEnv("devenv_open_folder", { installationId: local.installationId }), undefined);
  assert.throws(() => demoDevEnv("devenv_reinstall", { target }), /本地工具链不能/);
  assert.deepEqual((demoDevEnv("devenv_state", {}) as DevEnvState).installations, [local]);
  assert.equal((demoDevEnv("devenv_remove", { target, confirm: [] }) as DevEnvState).installations.length, 0);
});
