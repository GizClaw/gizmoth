import test from "node:test";
import assert from "node:assert/strict";
import { simulatorProfiles } from "../src/simulator-profiles.ts";

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
