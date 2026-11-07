import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const json = (path: string) => JSON.parse(readFileSync(new URL(path, import.meta.url), "utf8"));

test("isolated simulator capability has no device, provider, or project file commands", () => {
  const manifest = json("../src-tauri/permissions/application.json");
  const runtime = manifest.permission.find((p: { identifier: string }) => p.identifier === "simulator-runtime");
  assert.deepEqual([...runtime.commands.allow].sort(), [
    "simulator_package_put",
    "simulator_package_remove",
    "simulator_profile_update",
    "simulator_profiles_list",
  ]);
  const capability = json("../src-tauri/capabilities/simulator.json");
  assert.deepEqual(capability.windows, ["simulator-*"]);
  assert.deepEqual(capability.remote.urls, ["http://127.0.0.1:*"]);
  assert.ok(capability.permissions.includes("simulator-runtime"));
  assert.ok(!capability.permissions.includes("main-commands"));
});

test("main window retains credential permissions and its restrictive CSP", () => {
  const capability = json("../src-tauri/capabilities/default.json");
  assert.deepEqual(capability.windows, ["main"]);
  assert.equal(capability.remote, undefined);
  const manifest = json("../src-tauri/permissions/application.json");
  const commands = manifest.permission.find((p: { identifier: string }) => p.identifier === "main-commands").commands
    .allow;
  for (const command of ["provider_key", "device_key", "project_read", "project_write", "bind_device"])
    assert.ok(commands.includes(command));
  const csp = json("../src-tauri/tauri.conf.json").app.security.csp;
  assert.ok(csp.includes("script-src 'self';"));
  assert.ok(csp.includes("frame-src 'none'"));
  assert.ok(!csp.includes("wasm-unsafe-eval"));
});
