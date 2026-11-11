import test from "node:test";
import assert from "node:assert/strict";
import { createBrowserCatalog } from "../src/devenv-browser.ts";

const commit = "a".repeat(40);
const apiVersion = "devenv.gizmoth.dev/v1alpha1";
const repository = "https://github.com/GizClaw/gizmoth-devenv";

function fixture() {
  const files: Record<string, unknown> = {
    "registry.json": {
      apiVersion,
      kind: "Registry",
      repository,
      chips: ["esp32s3"],
      sdks: ["esp-idf"],
      toolchains: ["bazelisk"],
    },
    "chips/esp32s3/chip.json": {
      apiVersion,
      kind: "Chip",
      id: "esp32s3",
      name: "ESP32-S3",
      vendor: "Espressif",
      description: "ESP32-S3 SDK",
      homepage: "https://www.espressif.com/",
      sdk: "esp-idf",
      status: "planned",
      target: "esp32s3",
      tools: ["bazelisk"],
    },
    "sdks/esp-idf/sdk.json": {
      apiVersion,
      kind: "Sdk",
      id: "esp-idf",
      name: "ESP-IDF",
      status: "planned",
      releases: [],
    },
    "toolchains/bazelisk/toolchain.json": {
      apiVersion,
      kind: "Toolchain",
      id: "bazelisk",
      name: "Bazel (Bazelisk)",
      description: "Project-selected Bazel",
      category: "build-tool",
      homepage: "https://github.com/bazelbuild/bazelisk",
      status: "published",
      defaultVersion: "1.30.0",
      releases: [{ version: "1.30.0", manifest: "versions/1.30.0.json" }],
    },
  };
  const requests: string[] = [];
  let status = 200;
  const catalog = createBrowserCatalog(async (url, options) => {
    requests.push(url);
    assert.equal(options?.credentials, "omit");
    assert.ok(options?.signal);
    if (status !== 200) return new Response("Unavailable", { status });
    if (url.startsWith("https://api.github.com/")) return new Response(commit);
    const prefix = `https://raw.githubusercontent.com/GizClaw/gizmoth-devenv/${commit}/`;
    assert.ok(url.startsWith(prefix), `Catalog files must belong to the resolved commit: ${url}`);
    const path = url.slice(prefix.length);
    assert.ok(path in files, `Unexpected fetch: ${url}`);
    return new Response(JSON.stringify(files[path]));
  });
  return {
    catalog,
    files,
    requests,
    setStatus(value: number) {
      status = value;
    },
  };
}

test("ordinary browser reads real indexed metadata without simulating installed environments", async () => {
  const { catalog, requests } = fixture();
  const state = await catalog.state();
  assert.equal(state.registry?.commit, commit);
  assert.equal(state.chips[0].name, "ESP32-S3");
  assert.equal(state.chips[0].sdk, "ESP-IDF");
  assert.equal(state.chips[0].status, "planned");
  assert.equal(state.toolchains[0].version, "1.30.0");
  assert.equal(state.toolchains[0].status, "available");
  assert.deepEqual([state.host, state.installations, state.job], [null, [], null]);
  assert.equal(requests.length, 5);
  assert.ok(requests.every((url) => !url.includes("/versions/") && !url.includes("releases/download")));
});

test("navigation shares a cached snapshot and concurrent refreshes use one request", async () => {
  const { catalog, requests } = fixture();
  const [a, b] = await Promise.all([catalog.state(), catalog.state()]);
  assert.equal(a, b);
  assert.equal(requests.filter((url) => url.startsWith("https://api.github.com")).length, 1);
  await catalog.state();
  assert.equal(requests.length, 5);
});

test("a failed refresh preserves the loaded catalog and can be retried", async () => {
  const { catalog, setStatus } = fixture();
  const state = await catalog.state();
  setStatus(403);
  await assert.rejects(catalog.refresh(), /GitHub 请求过于频繁/);
  assert.equal(await catalog.state(), state);
  setStatus(200);
  assert.equal((await catalog.refresh()).chips.length, 1);
});

test("an initial network failure can be retried without leaving a poisoned pending request", async () => {
  const { catalog, setStatus } = fixture();
  setStatus(503);
  await assert.rejects(catalog.state(), /HTTP 503/);
  setStatus(200);
  assert.equal((await catalog.state()).chips[0].id, "esp32s3");
});

test("rejects a foreign registry and a path-traversal ID before following references", async () => {
  const foreign = fixture();
  (foreign.files["registry.json"] as Record<string, unknown>).repository = "https://github.com/other/repo";
  await assert.rejects(foreign.catalog.state(), /来源不匹配/);
  assert.equal(foreign.requests.length, 2);
  const traversal = fixture();
  (traversal.files["registry.json"] as Record<string, unknown>).chips = ["../outside"];
  await assert.rejects(traversal.catalog.state(), /格式无效/);
  assert.equal(traversal.requests.length, 2);
});

test("rejects mismatched identities and unresolved chip references instead of showing incomplete cards", async () => {
  const wrong = fixture();
  (wrong.files["chips/esp32s3/chip.json"] as Record<string, unknown>).id = "esp32c3";
  await assert.rejects(wrong.catalog.state(), /格式无效/);
  const missing = fixture();
  (missing.files["chips/esp32s3/chip.json"] as Record<string, unknown>).sdk = "missing";
  await assert.rejects(missing.catalog.state(), /引用缺失/);
});

test("rejects a published default version that is absent from the release index", async () => {
  const { catalog, files } = fixture();
  (files["toolchains/bazelisk/toolchain.json"] as Record<string, unknown>).defaultVersion = "2.0.0";
  await assert.rejects(catalog.state(), /默认版本未发布/);
});

test("browser catalog exposes Python versions and exact generic environment dependencies", async () => {
  const { catalog, files } = fixture();
  const index = files["registry.json"] as Record<string, unknown>;
  index.environments = ["gizos"];
  index.toolchains = ["bazelisk", "python"];
  files["toolchains/python/toolchain.json"] = {
    ...(files["toolchains/bazelisk/toolchain.json"] as Record<string, unknown>),
    id: "python",
    name: "Python",
    category: "runtime",
    defaultVersion: "3.12.16",
    releases: ["3.12.16", "3.11.18"].map((version) => ({ version, manifest: `versions/${version}.json` })),
  };
  files["environments/gizos/environment.json"] = {
    apiVersion,
    kind: "Environment",
    id: "gizos",
    name: "GizOS",
    description: "Host tools",
    homepage: "https://github.com/GizClaw/gizos",
    status: "published",
    dependencies: [
      { toolchain: "python", version: "3.11.18" },
      { toolchain: "python", version: "3.12.16" },
    ],
  };
  const state = await catalog.state();
  assert.deepEqual(
    state.toolchains.find((item) => item.id === "python")!.versions.map((item) => item.version),
    ["3.12.16", "3.11.18"],
  );
  assert.deepEqual(
    state.environments[0].packages.map((item) => [item.id, item.version]),
    [
      ["python", "3.11.18"],
      ["python", "3.12.16"],
    ],
  );
  assert.deepEqual(state.installations, []);
  (files["environments/gizos/environment.json"] as Record<string, unknown>).dependencies = [
    { toolchain: "python", version: "9.9.9" },
  ];
  await assert.rejects(catalog.refresh(), /引用缺失/);
});

test("planned environments can name unpublished versions of known tools without breaking browsing", async () => {
  const { catalog, files } = fixture();
  (files["registry.json"] as Record<string, unknown>).environments = ["future-env"];
  const future = {
    apiVersion,
    kind: "Environment",
    id: "future-env",
    name: "Future environment",
    description: "Planned host tools",
    homepage: "https://example.com/",
    status: "planned",
    dependencies: [{ toolchain: "bazelisk", version: "99.0.0" }],
  };
  files["environments/future-env/environment.json"] = future;
  const state = await catalog.state();
  assert.equal(state.chips.length, 1);
  assert.equal(state.toolchains[0].status, "available");
  assert.equal(state.environments[0].status, "planned");
  assert.equal(state.environments[0].packages[0].version, "99.0.0");
  assert.equal(state.environments[0].packages[0].installed, false);

  future.status = "published";
  await assert.rejects(catalog.refresh(), /引用缺失/);
  future.status = "planned";
  future.dependencies[0].toolchain = "missing-tool";
  await assert.rejects(catalog.refresh(), /引用缺失/);
  assert.equal(await catalog.state(), state, "invalid refreshes preserve the previously usable catalog");
});
