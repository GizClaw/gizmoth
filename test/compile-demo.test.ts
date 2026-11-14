import test, { type TestContext } from "node:test";
import assert from "node:assert/strict";
import type { CompileArtifact, CompileJob, CompileSnapshot, LogChunk } from "../src/compile.ts";

const START = 50_000_000;

async function fixture(t: TestContext, name: string) {
  t.mock.timers.enable({ apis: ["Date"], now: START });
  const module = await import(new URL(`../src/demo-compile.ts?test=${name}`, import.meta.url).href);
  const request = (method: string, path: string, body?: unknown) =>
    module.demoCompile("compile_request", { method, path, body: body ?? null });
  const snapshot = () => request("GET", "/v1/compile") as CompileSnapshot;
  const job = (title: string) => snapshot().jobs.find((item: CompileJob) => item.title === title)!;
  const at = (ms: number) => t.mock.timers.setTime(START + ms);
  // The script starts with the first compile request; anchor it at START.
  snapshot();
  return { module, request, snapshot, job, at };
}

test("the demo script starts with the first compile request, not when the bridge loads", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: START });
  const { demoCompile } = await import(new URL("../src/demo-compile.ts?test=lazy", import.meta.url).href);
  t.mock.timers.setTime(START + 600_000);
  const state = demoCompile("compile_request", { method: "GET", path: "/v1/compile" }) as CompileSnapshot;
  assert.deepEqual(
    state.slots.map((slot) => slot.state),
    ["busy", "busy"],
  );
});

test("demo compile service starts with busy slots, a queue, shared caches and artifacts", async (t) => {
  const { request, snapshot } = await fixture(t, "initial");
  const state = snapshot();
  assert.equal(state.protocol, "gizmoth.compile.v1");
  assert.deepEqual(
    state.slots.map((slot) => [slot.name, slot.state, slot.job?.client]),
    [
      ["Slot 1", "busy", "claude-code"],
      ["Slot 2", "busy", "codex"],
    ],
  );
  assert.deepEqual(
    state.queue.map((item) => [item.title, item.status, item.position]),
    [["Build BK7258 firmware", "queued", 1]],
  );
  assert.deepEqual(
    state.caches.map((cache) => [cache.name, cache.kind]),
    [
      ["bazel", "bazel"],
      ["ccache", "ccache"],
      ["idf-components", "custom"],
    ],
  );
  assert.equal(state.image.status, "ready");
  assert.equal(state.image.current, true);
  // Settings use the Rust serde names; slot views use MiB/GiB spellings.
  assert.deepEqual(Object.keys(state.settings.slots[0]).sort(), ["cpus", "diskGib", "id", "memoryMib", "name"]);
  assert.equal(state.slots[0].memoryMiB, 6144);
  const artifacts = request("GET", "/v1/compile/artifacts") as CompileArtifact[];
  assert.equal(state.artifacts.count, artifacts.length);
  assert.ok(
    artifacts.every((artifact) => artifact.files.every((file) => file.absolutePath === `${artifact.dir}/${file.path}`)),
  );
  assert.equal(artifacts.filter((artifact) => artifact.pinned).length, 1);
  assert.ok(artifacts[0].createdAt >= artifacts[1].createdAt);
});

test("builds progress, collect artifacts, free slots and dispatch the queue", async (t) => {
  const { request, snapshot, job, at } = await fixture(t, "timeline");
  at(5_000);
  const running = job("Build AMOLED firmware for review");
  assert.equal(running.status, "running");
  assert.equal(running.synced?.files, 3);
  const log = request("GET", `/v1/compile/jobs/${running.id}/log`) as LogChunk;
  assert.match(log.text, /\x1b\[2m\[gizmoth\] Running on Slot 1/);
  assert.match(log.text, /Synchronizing \/Users\/demo\/Projects\/amoled-firmware/);

  // Paging continues from the returned offset without repeating text.
  const first = request("GET", `/v1/compile/jobs/${running.id}/log?after=0&limit=40`) as LogChunk;
  const second = request("GET", `/v1/compile/jobs/${running.id}/log?after=${first.next}&limit=40`) as LogChunk;
  assert.equal(first.text.length, 40);
  assert.equal(first.text + second.text, log.text.slice(0, first.text.length + second.text.length));

  at(41_000);
  const bk = job("Build BK7258 firmware");
  assert.equal(job("Run protocol unit tests").status, "succeeded");
  assert.notEqual(bk.status, "queued");
  assert.equal(bk.slot, snapshot().slots[1].id);
  assert.equal(snapshot().queue.length, 0);

  at(80_000);
  const finished = job("Build AMOLED firmware for review");
  assert.equal(finished.status, "succeeded");
  assert.deepEqual(finished.exit, { code: 0, signal: null, timedOut: false, cancelled: false });
  assert.ok(finished.artifact);
  const artifact = request("GET", `/v1/compile/artifacts/${finished.artifact}`) as CompileArtifact;
  assert.deepEqual(
    artifact.files.map((file) => file.path),
    ["bazel-bin/app/firmware.update.tar.zlib", "bazel-bin/app/firmware.elf"],
  );
  assert.equal(job("Build BK7258 firmware").status, "failed");
  assert.equal(job("Build BK7258 firmware").exit?.code, 1);
  assert.match(job("Build BK7258 firmware").error ?? "", /exited with code 1/);
  assert.deepEqual(
    snapshot().slots.map((slot) => [slot.state, slot.job]),
    [
      ["ready", null],
      ["ready", null],
    ],
  );
  assert.equal(snapshot().artifacts.count, 3);
});

test("cancelling a queued build is immediate; a running build stops at its next boundary", async (t) => {
  const { request, job, at } = await fixture(t, "cancel");
  const queued = job("Build BK7258 firmware");
  assert.equal((request("POST", `/v1/compile/jobs/${queued.id}/cancel`) as CompileJob).status, "cancelled");
  at(3_000);
  const running = job("Build AMOLED firmware for review");
  request("POST", `/v1/compile/jobs/${running.id}/cancel`);
  assert.equal(job(running.title).status, "running");
  at(4_300);
  const cancelled = job(running.title);
  assert.equal(cancelled.status, "cancelled");
  assert.equal(cancelled.exit?.cancelled, true);
  assert.equal(cancelled.artifact, null);
  at(45_000);
  // The freed slots never pick the cancelled queued build back up.
  assert.equal(job("Build BK7258 firmware").status, "cancelled");
});

test("slots are validated and only change resources or disks while stopped", async (t) => {
  const { request, snapshot, at } = await fixture(t, "slots");
  const [busy] = snapshot().slots;
  assert.throws(() => request("POST", "/v1/compile/slots", { cpus: 99 }), /CPUs/);
  assert.throws(() => request("POST", "/v1/compile/slots", { memoryMiB: 512 }), /at least 1 GiB/);
  const added = request("POST", "/v1/compile/slots", {
    name: "Slot 3",
    cpus: 2,
    memoryMiB: 4096,
    diskGiB: 32,
  }) as CompileSnapshot;
  assert.deepEqual(
    added.slots.map((slot) => slot.state),
    ["busy", "busy", "stopped"],
  );
  assert.throws(() => request("PATCH", `/v1/compile/slots/${busy.id}`, { cpus: 2 }), /Stop the slot/);
  assert.equal(
    (request("PATCH", `/v1/compile/slots/${busy.id}`, { name: "Firmware" }) as CompileSnapshot).slots[0].name,
    "Firmware",
  );
  assert.throws(() => request("DELETE", `/v1/compile/slots/${busy.id}`), /cancel it first/);
  assert.throws(() => request("POST", `/v1/compile/slots/${busy.id}/reset`), /cancel it first/);

  at(80_000);
  assert.equal(snapshot().slots[0].state, "ready");
  assert.equal((request("POST", `/v1/compile/slots/${busy.id}/stop`) as CompileSnapshot).slots[0].state, "stopping");
  at(82_000);
  assert.equal(snapshot().slots[0].state, "stopped");
  assert.throws(() => request("PATCH", `/v1/compile/slots/${busy.id}`, { diskGiB: 128 }), /Reset the work disk/);
  const reset = request("POST", `/v1/compile/slots/${busy.id}/reset`) as CompileSnapshot;
  assert.equal(reset.slots[0].disk.allocated, 0);
  assert.equal(
    (request("PATCH", `/v1/compile/slots/${busy.id}`, { diskGiB: 128, cpus: 8 }) as CompileSnapshot).slots[0].diskGiB,
    128,
  );
  const removed = request("DELETE", `/v1/compile/slots/${busy.id}`) as CompileSnapshot;
  assert.equal(removed.slots.length, 2);
  assert.equal(removed.settings.slots.length, 2);
});

test("settings patches validate; the image packages are fixed and it rebuilds", async (t) => {
  const { request, snapshot, at } = await fixture(t, "image");
  assert.throws(() => request("PATCH", "/v1/compile/settings", { slots: [] }), /slot endpoints/);
  assert.throws(() => request("PATCH", "/v1/compile/settings", { typo: 1 }), /Unknown setting/);
  // The package list is fixed; the mirror is only a download source.
  assert.ok(snapshot().image.packages.includes("erofs-utils"));
  assert.throws(() => request("PATCH", "/v1/compile/settings", { image: { aptMirror: "ftp://x" } }), /URL/);
  const mirror = "http://mirrors.ustc.edu.cn/ubuntu-ports";
  const patched = request("PATCH", "/v1/compile/settings", { image: { aptMirror: mirror } }) as CompileSnapshot;
  assert.equal(patched.image.current, true);
  assert.deepEqual(patched.settings.image, { aptMirror: mirror });
  assert.throws(() => request("POST", "/v1/compile/image/build"), /Wait for running builds/);

  at(80_000);
  const building = request("POST", "/v1/compile/image/build") as CompileSnapshot;
  assert.equal(building.image.status, "building");
  assert.match(building.image.phase ?? "", /^Downloading Linux/);
  at(81_000);
  const progress = snapshot().image;
  assert.ok(progress.progress && progress.progress.done > 0 && progress.progress.done < progress.progress.total);
  assert.match((request("GET", "/v1/compile/image/log") as LogChunk).text, /Linux 6\.18/);
  at(93_000);
  const ready = snapshot().image;
  assert.equal(ready.status, "ready");
  assert.equal(ready.current, true);
  assert.equal(ready.info?.aptMirror, mirror);
  assert.deepEqual(ready.info?.packages, ready.packages);

  request("POST", "/v1/compile/image/build");
  request("POST", "/v1/compile/image/cancel");
  at(93_700);
  assert.deepEqual([snapshot().image.status, snapshot().image.error], ["ready", "Cancelled"]);
});

test("caches clear and gain custom namespaces; artifacts pin, prune and delete", async (t) => {
  const { module, request, snapshot, at } = await fixture(t, "manage");
  const cleared = request("DELETE", "/v1/compile/caches/ccache") as CompileSnapshot;
  assert.equal(cleared.caches.find((cache) => cache.name === "ccache")?.bytes, 0);
  const caches = structuredClone(snapshot().settings.caches);
  caches.custom.push({ name: "sccache", env: "SCCACHE_WEBDAV_ENDPOINT", maxGib: 5 });
  caches.bazel.maxGib = 80;
  const updated = request("PATCH", "/v1/compile/settings", { caches }) as CompileSnapshot;
  assert.deepEqual(
    updated.caches.map((cache) => [cache.name, cache.maxBytes / 1024 ** 3, cache.env]),
    [
      ["bazel", 80, null],
      ["ccache", 20, null],
      ["idf-components", 10, "IDF_COMPONENT_CACHE_URL"],
      ["sccache", 5, "SCCACHE_WEBDAV_ENDPOINT"],
    ],
  );
  caches.custom.push({ name: "bazel", env: "X", maxGib: 1 });
  assert.throws(() => request("PATCH", "/v1/compile/settings", { caches }), /duplicate cache name/);

  at(80_000);
  const list = () => request("GET", "/v1/compile/artifacts") as CompileArtifact[];
  assert.equal(list().length, 3);
  // Keeping one unpinned artifact removes the oldest unpinned one only.
  request("PATCH", "/v1/compile/settings", { artifacts: { keepLast: 1, maxGib: 20 } });
  assert.equal(list().length, 2);
  assert.equal(list().filter((artifact) => artifact.pinned).length, 1);
  const [newest] = list();
  assert.equal(
    (request("PATCH", `/v1/compile/artifacts/${newest.id}`, { pinned: true }) as CompileArtifact).pinned,
    true,
  );
  assert.throws(() => request("PATCH", `/v1/compile/artifacts/${newest.id}`, { pinned: "yes" }), /boolean/);
  assert.deepEqual(request("DELETE", `/v1/compile/artifacts/${newest.id}`), { deleted: newest.id });
  assert.throws(() => request("GET", `/v1/compile/artifacts/${newest.id}`), /not found/);
  assert.equal(snapshot().artifacts.count, 1);

  module.demoCompile("compile_reveal", { path: newest.dir });
  assert.deepEqual(module.revealed, [newest.dir]);
  assert.throws(
    () => module.demoCompile("compile_request", { method: "GET", path: "/v1/ports" }),
    /Invalid compile path/,
  );
});

test("the working directory moves after builds finish and blocks changes meanwhile", async (t) => {
  const { module, request, snapshot, at } = await fixture(t, "storage");
  const initial = snapshot().storage;
  assert.equal(initial.isDefault, true);
  assert.equal(initial.root, initial.default);
  const target = module.demoCompile("compile_choose_directory", {}) as string;
  assert.throws(() => request("POST", "/v1/compile/storage", { path: target }), /Wait for builds to finish/);
  for (const job of snapshot().jobs) request("POST", `/v1/compile/jobs/${job.id}/cancel`);
  at(120_000);
  assert.equal(
    snapshot().jobs.filter((job) => ["queued", "booting", "syncing", "running", "collecting"].includes(job.status))
      .length,
    0,
  );
  assert.throws(() => request("POST", "/v1/compile/storage", { path: "relative" }), /absolute path/);
  assert.throws(() => request("POST", "/v1/compile/storage", { path: `${initial.root}/inner` }), /inside the current/);
  const moving = (request("POST", "/v1/compile/storage", { path: target }) as CompileSnapshot).storage.moving;
  assert.equal(moving?.to, target);
  assert.throws(() => request("POST", "/v1/compile/slots", {}), /being moved/);
  at(122_000);
  const halfway = snapshot().storage.moving!;
  assert.ok(halfway.done > 0 && halfway.done < halfway.total);
  at(125_000);
  const moved = snapshot();
  assert.equal(moved.storage.moving, null);
  assert.equal(moved.storage.root, target);
  assert.equal(moved.storage.isDefault, false);
  assert.equal(moved.artifacts.dir, `${target}/artifacts`);
  request("POST", "/v1/compile/storage", { default: true });
  request("POST", "/v1/compile/storage/cancel");
  assert.deepEqual([snapshot().storage.root, snapshot().storage.error], [target, "cancelled"]);
});

test("every registry package downloads and packs on its own; nothing is chosen", async (t) => {
  const { request, snapshot, at } = await fixture(t, "packages");
  const initial = snapshot().packages;
  assert.equal(initial.active, true);
  assert.deepEqual(
    initial.items.map((item) => [item.id, item.version, item.default, item.state]),
    [
      ["arm-gnu", "14.3.rel1", true, "downloading"],
      ["bazelisk", "1.30.0", true, "installed"],
      ["bazelisk", "1.29.0", false, "installed"],
      ["python", "3.12.16", true, "installed"],
      ["python", "3.11.18", false, "installed"],
    ],
  );
  assert.equal(initial.pack.state, "stale");
  assert.equal(initial.items[3].path, "/opt/gizmoth/packages/toolchains/python/3.12.16");
  at(10_000);
  const halfway = snapshot().packages.items[0];
  assert.ok(halfway.done! > 0 && halfway.done! < halfway.total!);
  // Moving the working directory waits for this (after running builds).
  assert.equal(snapshot().packages.busy, true);
  at(21_000);
  assert.deepEqual([snapshot().packages.items[0].state, snapshot().packages.pack.state], ["packing", "packing"]);
  at(25_000);
  const done = snapshot().packages;
  assert.ok(done.items.every((item) => item.state === "installed"));
  assert.equal(done.pack.state, "ready");
  assert.equal(
    snapshot().storage.items.find((item) => item.id === "packages")?.bytes,
    done.pack.state === "ready" ? done.pack.bytes : 0,
  );
  const refreshing = (request("POST", "/v1/compile/packages/refresh") as CompileSnapshot).packages.registry;
  assert.equal(refreshing.refreshing, true);
  at(27_000);
  assert.equal(snapshot().packages.registry.refreshing, false);
  assert.equal(snapshot().packages.registry.fetchedAt, START + 26_500);
});
