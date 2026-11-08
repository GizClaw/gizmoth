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
    "simulator_profile_delete",
    "simulator_profile_update",
    "simulator_profiles_list",
    "simulator_window_destroy",
  ]);
  const capability = json("../src-tauri/capabilities/simulator.json");
  assert.deepEqual(capability.windows, ["simulator-*"]);
  assert.deepEqual(capability.remote.urls, ["http://127.0.0.1:*"]);
  assert.ok(capability.permissions.includes("simulator-runtime"));
  assert.ok(!capability.permissions.includes("main-commands"));
  assert.ok(!capability.permissions.includes("core:window:allow-destroy"));
  const fixture = json("../src-tauri/tauri.e2e.conf.json").app.security.capabilities.find(
    (entry: { identifier: string }) => entry.identifier === "native-e2e-simulator",
  );
  assert.ok(!fixture.permissions.includes("core:window:allow-destroy"));
});

test("simulator allows the default Trial endpoint without opening arbitrary network origins", () => {
  const source = readFileSync(new URL("../src-tauri/src/simulator.rs", import.meta.url), "utf8");
  const policy = source.match(/SIMULATOR_CSP: &str = "([^"]+)"/)?.[1];
  assert.ok(policy);
  const connect = policy
    .split(";")
    .find((directive) => directive.trim().startsWith("connect-src "))!
    .trim()
    .split(/\s+/);
  assert.ok(
    connect.includes("https://ap.trial.gizclaw.com"),
    "Trial firmware loads /server-info from this exact origin",
  );
  assert.ok(!connect.includes("*") && !connect.includes("https:") && !connect.includes("http:"));
  assert.ok(!json("../src-tauri/tauri.conf.json").app.security.csp.includes("ap.trial.gizclaw.com"));
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
