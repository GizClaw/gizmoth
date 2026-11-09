import test from "node:test";
import assert from "node:assert/strict";
import { createSimulatorTargets } from "../src/simulator-target.ts";

test("a late simulator open cannot replace a newer hardware selection", () => {
  const targets = createSimulatorTargets();
  const opening = targets.begin("project");
  assert.equal(targets.isCurrent("project", opening), true);
  targets.clear("project");
  assert.equal(targets.isCurrent("project", opening), false);
  assert.equal(targets.commit("project", opening, "old-simulator"), false);
  assert.equal(targets.current("project"), undefined);
});

test("newer opens win even if older opens complete later", () => {
  const targets = createSimulatorTargets();
  const first = targets.begin("project"),
    second = targets.begin("project");
  assert.equal(targets.commit("project", second, "new-simulator"), true);
  assert.equal(targets.commit("project", first, "old-simulator"), false);
  assert.equal(targets.current("project"), "new-simulator");
});

test("failed or canceled recovery preserves local routing intent and project isolation", () => {
  const targets = createSimulatorTargets();
  targets.commit("project", targets.begin("project"), "deleted-simulator");
  const opening = targets.begin("project");
  targets.begin("project"); // Invalidate a canceled open without selecting hardware.
  assert.equal(targets.commit("project", opening, "canceled"), false);
  assert.equal(targets.current("project"), "deleted-simulator");
  const other = targets.begin("other-project");
  targets.clear("project");
  assert.equal(targets.commit("other-project", other, "other-simulator"), true);
});
