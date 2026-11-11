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
