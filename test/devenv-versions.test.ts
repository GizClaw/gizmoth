import test from "node:test";
import assert from "node:assert/strict";
import { selectedVersion, uninstalledVersions } from "../src/devenv-versions.ts";
import type { DevEnvInstallation, DevEnvToolchain } from "../src/devenv.ts";

const python: DevEnvToolchain = {
  id: "python",
  name: "Python",
  description: "Python runtime",
  homepage: "https://www.python.org/",
  notes: null,
  category: "runtime",
  status: "installed",
  blocked: null,
  reason: null,
  version: "3.12.16",
  versions: ["3.12.16", "3.11.18"].map((version) => ({ version, status: "available", blocked: null, reason: null })),
};
function installed(id: string, version: string): DevEnvInstallation {
  return {
    installationId: `${id}@${version}`,
    kind: "toolchain",
    id,
    name: id,
    version,
    category: "runtime",
    origin: "managed",
    requestedByUser: true,
    path: `/example/${id}/${version}`,
    chips: [],
    environments: [],
    healthy: true,
    reinstallable: true,
  };
}

test("installing the default leaves older Python releases visible for installation", () => {
  const remaining = uninstalledVersions(python, [installed("python", "3.12.16")]);
  assert.deepEqual(
    remaining.map((release) => release.version),
    ["3.11.18"],
  );
  assert.equal(selectedVersion(remaining, "3.12.16", python.version)?.version, "3.11.18");
});

test("a version selector respects explicit choices and only hides exact installed identities", () => {
  const remaining = uninstalledVersions(python, [installed("other-tool", "3.12.16")]);
  assert.equal(remaining.length, 2);
  assert.equal(selectedVersion(remaining, "3.11.18", python.version)?.version, "3.11.18");
  assert.equal(selectedVersion(remaining, undefined, python.version)?.version, "3.12.16");
  assert.deepEqual(uninstalledVersions(python, [installed("python", "3.12.16"), installed("python", "3.11.18")]), []);
  assert.equal(selectedVersion([], "3.11.18", python.version), null);
});

test("demo generic environments pin one Python version while another coexists independently", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 1_000 });
  const { demoDevEnv } = await import(new URL("../src/demo-devenv.ts?test=environment-versions", import.meta.url).href);
  const read = () => demoDevEnv("devenv_state", {}) as import("../src/devenv.ts").DevEnvState;
  demoDevEnv("devenv_install", { target: { kind: "toolchain", id: "python", version: "3.11.18" } });
  t.mock.timers.tick(5_000);
  const old = read().installations[0];
  demoDevEnv("devenv_install", { target: { kind: "environment", id: "gizos" } });
  t.mock.timers.tick(5_000);
  const state = read();
  assert.equal(state.environments[0].status, "installed");
  const runtimes = state.installations.filter((record) => record.id === "python");
  assert.deepEqual(runtimes.map((record) => record.version).sort(), ["3.11.18", "3.12.16"]);
  assert.notEqual(runtimes[0].path, runtimes[1].path);
  const pinned = runtimes.find((record) => record.version === "3.12.16")!;
  const target = { kind: "installation", id: pinned.installationId };
  const blocked = demoDevEnv("devenv_remove_preview", { target }) as import("../src/devenv.ts").RemovalPreview;
  assert.deepEqual(blocked.environments, ["GizOS"]);
  assert.equal(blocked.action, "blocked");

  demoDevEnv("devenv_reinstall", { target });
  t.mock.timers.tick(5_000);
  const repaired = read();
  assert.equal(repaired.installations.length, 3);
  const updated = repaired.installations.find((record) => record.id === "python" && record.version === "3.12.16")!;
  assert.notEqual(updated.installationId, pinned.installationId);
  assert.deepEqual(updated.environments, ["GizOS"]);

  const environment = { kind: "environment", id: "gizos" };
  const removal = demoDevEnv("devenv_remove_preview", {
    target: environment,
  }) as import("../src/devenv.ts").RemovalPreview;
  demoDevEnv("devenv_remove", { target: environment, confirm: removal.confirm });
  assert.deepEqual(read().installations, [old]);
  assert.equal(read().environments[0].status, "available");
});
