import test from "node:test";
import assert from "node:assert/strict";
import { selectSimulatorProfile, simulatorProfiles, type SimulatorProfile } from "../src/simulator-profiles.ts";

test("profile preferences persist independently of projects and cannot resurrect after deletion", async () => {
  const values = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    },
  });
  Object.defineProperty(globalThis, "window", { configurable: true, value: new EventTarget() });
  const profile = await simulatorProfiles.create("  Desk  ", "h106-zero");
  assert.equal(profile.name, "Desk");
  await simulatorProfiles.update(profile.id, { firmwareId: "zero-trial", view: "screen" });
  const saved = (await simulatorProfiles.list())[0];
  assert.equal(saved.id, profile.id);
  assert.equal(saved.model, "h106-zero");
  assert.equal(saved.firmwareId, "zero-trial");
  assert.equal(saved.view, "screen");
  assert.deepEqual(Object.keys(saved).sort(), ["firmwareId", "id", "model", "name", "view"]);
  await assert.rejects(simulatorProfiles.update(profile.id, { name: "x".repeat(81) }));
  await assert.rejects(simulatorProfiles.update(profile.id, { model: "h106-tiga" } as never), /创建后不可更改/);
  assert.equal((await simulatorProfiles.list())[0].model, "h106-zero");
  await assert.rejects(simulatorProfiles.create("Invalid", "unknown" as never));
  // Legacy profiles survive migration; only the obsolete relationship is discarded.
  values.set("gizmoth.simulators.v1", JSON.stringify([{ ...saved, projectId: "old-project" }]));
  assert.deepEqual((await simulatorProfiles.list())[0], saved);
  await simulatorProfiles.update(profile.id, { name: "Renamed" });
  assert.equal(JSON.parse(values.get("gizmoth.simulators.v1")!)[0].projectId, undefined);
  assert.equal((await simulatorProfiles.list())[0].firmwareId, "zero-trial");
  await simulatorProfiles.delete(profile.id);
  await assert.rejects(simulatorProfiles.update(profile.id, { view: "model" }), /已删除/);
  assert.deepEqual(await simulatorProfiles.list(), []);
});

const profile = (id: string): SimulatorProfile => ({
  id,
  name: id,
  model: "h106-tiga",
  firmwareId: "",
  view: "screen",
});

test("explicit simulator IDs never silently choose another profile", () => {
  const first = profile("first"),
    second = profile("second");
  assert.equal(selectSimulatorProfile([first, second], "second", "first"), second);
  assert.throws(() => selectSimulatorProfile([first], "deleted", "first"), /Simulator does not exist/);
  assert.throws(() => selectSimulatorProfile([first], "", "first"), /Simulator does not exist/);
});

test("a missing remembered simulator falls back to a surviving profile", () => {
  const first = profile("first"),
    second = profile("second");
  assert.equal(selectSimulatorProfile([first, second], undefined, "deleted"), first);
  assert.equal(selectSimulatorProfile([first, second], undefined, "second"), second);
  assert.equal(selectSimulatorProfile([first, second]), first);
});

test("deleting the last implicit simulator leaves the creation path reachable", () => {
  assert.equal(selectSimulatorProfile([], undefined, "deleted"), undefined);
  assert.equal(selectSimulatorProfile([]), undefined);
  assert.throws(() => selectSimulatorProfile([], "deleted", "deleted"), /Simulator does not exist/);
});
