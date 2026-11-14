import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { waitFor, withApp } from "./native-app.mjs";

const exec = promisify(execFile);
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Stops the fixture's service, which also powers off its VMs. */
async function stopService(data) {
  let info;
  try {
    info = JSON.parse(await readFile(join(data, "serial", "daemon.json"), "utf8"));
  } catch {
    return;
  }
  await fetch(`http://127.0.0.1:${info.port}/v1/shutdown`, {
    method: "POST",
    headers: { Authorization: `Bearer ${info.token}` },
  }).catch(() => {});
  for (let i = 0; i < 120; i++) {
    try {
      process.kill(info.pid, 0);
    } catch {
      return;
    }
    await pause(500);
  }
  process.kill(info.pid, "SIGKILL");
}

/** One build on its own compile socket; resolves with the final frame and log. */
function build(info, params, title, { closeWhen } = {}) {
  return new Promise((resolve, reject) => {
    const query = new URLSearchParams({ client: "native-e2e", title, token: info.token });
    const socket = new WebSocket(`ws://127.0.0.1:${info.port}/v1/compile/ws?${query}`);
    socket.binaryType = "arraybuffer";
    let log = "";
    let finished = null;
    socket.onopen = () => socket.send(JSON.stringify({ id: "build-1", method: "build", params }));
    socket.onmessage = async (event) => {
      if (typeof event.data !== "string") {
        log += Buffer.from(event.data).toString();
        return;
      }
      const frame = JSON.parse(event.data);
      if (frame.type === "result" && !frame.ok) reject(new Error(JSON.stringify(frame.error)));
      if (frame.type === "finished") finished = frame;
      if (closeWhen && (await closeWhen(frame))) socket.close();
    };
    socket.onerror = () => {};
    socket.onclose = () => resolve({ finished, log });
  });
}

// Opt-in: boots real Linux VMs through OpenVMM. Needs macOS on Apple silicon with
// `npm run compile-agent:build`, `npm run openvmm:build` and
// `npm run linux-image:build` staged. Without GIZMOTH_E2E_COMPILE_SEED (another
// service's `<app data>/compile`), the first build also downloads and packs the
// development packages.
test(
  "native Compiling page follows real slots, builds, cancellation and artifacts",
  {
    skip: process.env.GIZMOTH_LIVE_COMPILE_TEST !== "1" || process.platform !== "darwin" || process.arch !== "arm64",
    timeout: 900000,
  },
  (t) =>
    withApp(t, async (app) => {
      try {
        // The fixture's service must never listen on the LAN.
        const apiKey = `gmk_${randomUUID().replaceAll("-", "")}`;
        await writeFile(
          join(app.data, "gateway.json"),
          JSON.stringify({ enabled: false, scope: "local", port: 7420, apiKey }),
        );
        const seed = process.env.GIZMOTH_E2E_COMPILE_SEED;
        if (seed) {
          await mkdir(join(app.data, "compile"), { recursive: true });
          for (const item of ["assets", "packages.json", "packages.erofs", "packages.erofs.json"]) {
            // APFS clones cost no extra space.
            await exec("cp", ["-cR", join(seed, item), join(app.data, "compile", item)]).catch(() => {});
          }
        }
        const workspace = join(app.temporary, "workspace");
        await mkdir(join(workspace, "src"), { recursive: true });
        await writeFile(
          join(workspace, "src/main.c"),
          '#include <stdio.h>\nint main(void){puts("native e2e");return 0;}\n',
        );
        await writeFile(
          join(workspace, "Makefile"),
          "out/app.update.tar.zlib: src/main.c\n\tmkdir -p out\n\tgcc -O2 -o out/app src/main.c\n\tcp out/app $@\n",
        );
        const snapshot = async () => {
          const result = await app.invoke("compile_request", { method: "GET", path: "/v1/compile", body: null });
          assert.equal(result.ok, true, JSON.stringify(result));
          return result.value;
        };
        const page = (selector) =>
          app.browser.execute((selector) => document.querySelector(selector)?.textContent ?? "", selector);

        // Opening the page starts the fixture's service and reads it live.
        await app.browser.$('button[data-page="compile-builds"]').click();
        await app.browser.$(".compile-slots").waitForDisplayed({ timeout: 60000 });
        const initial = await snapshot();
        assert.equal(initial.available.ok, true, initial.available.message);
        assert.equal(initial.slots.length, 1);
        assert.equal(await app.browser.$(".compile-unavailable").isExisting(), false);
        // Views are second-level sidebar entries under 编译, not page tabs.
        assert.equal((await app.browser.$$("#nav-compiling .nav-child")).length, 3);
        assert.equal(await app.browser.$('[role="tablist"]').isExisting(), false);

        // Add a second slot from the page.
        await app.browser.$("button*=添加 slot").click();
        const name = await app.browser.$('input[id$="-name"]');
        await name.waitForDisplayed();
        await name.setValue("E2E Slot");
        await app.browser.$('button[type="submit"]').click();
        await waitFor(async () => (await snapshot()).slots.length === 2, "slot added through the page");
        await waitFor(async () => (await app.browser.$$(".compile-slot")).length === 2, "second slot card", 20000);
        assert.match(await page(".compile-slots"), /E2E Slot/);

        // A real build submitted by an agent appears live, then finishes with an artifact.
        const info = JSON.parse(await readFile(join(app.data, "serial", "daemon.json"), "utf8"));
        const make = build(
          info,
          { workspace, command: ["make"], artifacts: ["out/*.update.tar.zlib"] },
          "E2E native build",
        );
        await waitFor(
          async () => (await page(".compile-jobs")).includes("E2E native build"),
          "build in the page",
          30000,
        );
        await app.screenshot("compile-build-running");
        const result = await make;
        assert.equal(result.finished?.job.status, "succeeded", result.log);
        const artifact = result.finished.artifact;
        assert.deepEqual(
          artifact.files.map((file) => file.path),
          ["out/app.update.tar.zlib"],
        );
        await waitFor(
          async () =>
            (await snapshot()).jobs.some((job) => job.title === "E2E native build" && job.status === "succeeded"),
          "finished build",
        );
        // The expanded row streams the job log from the service.
        await app.browser.$("button.compile-job-row*=E2E native build").click();
        await waitFor(async () => (await page(".compile-job.open")).includes("gcc -O2"), "job log in the page", 20000);
        await app.screenshot("compile-build-log");

        // Closing an agent's socket cancels its running build, visible in the page.
        const cancelled = build(
          info,
          { workspace, command: ["bash", "-lc", "echo started; sleep 120"] },
          "E2E cancel on close",
          { closeWhen: (frame) => frame.type === "status" && frame.status === "running" },
        );
        await cancelled;
        await waitFor(
          async () =>
            (await snapshot()).jobs.some((job) => job.title === "E2E cancel on close" && job.status === "cancelled"),
          "cancelled build",
          30000,
        );
        await waitFor(async () => (await page(".compile-jobs")).includes("E2E cancel on close"), "cancelled build row");

        // Artifacts: listed with paths, pinned from the page.
        await app.browser.$('button[data-page="compile-artifacts"]').click();
        const row = await app.browser.$(`#artifact-${artifact.id}`);
        await row.waitForDisplayed({ timeout: 20000 });
        // One compact row per artifact; details and actions open in a dialog.
        assert.equal(await row.$("table").isExisting(), false);
        await row.$(".compile-artifact-row").click();
        const dialog = await app.browser.$(".compile-artifact-dialog");
        await dialog.waitForDisplayed();
        assert.match(await dialog.getText(), new RegExp(artifact.id));
        await dialog.$("button=固定").click();
        await waitFor(async () => {
          const list = await app.invoke("compile_request", {
            method: "GET",
            path: "/v1/compile/artifacts",
            body: null,
          });
          return list.ok && list.value.find((item) => item.id === artifact.id)?.pinned === true;
        }, "artifact pinned through the page");
        await waitFor(async () => /取消固定/.test(await dialog.getText()), "pinned state in the dialog");
        await app.screenshot("compile-artifacts");

        // Caches: Bazel and ccache rows from the host cache store.
        await app.browser.$('button[data-page="compile-caches"]').click();
        await waitFor(async () => /ccache/.test(await page(".compile-panel")), "cache rows");
        await app.screenshot("compile-caches");

        // Settings › 编译: the bundled image and the automatic registry packages.
        await app.browser.$('button[data-page="settings"]').click();
        await app.browser.$('[data-settings-section="compiling"]').click();
        await app.browser.$(".compile-settings").waitForDisplayed();
        assert.match(await page(".compile-image"), /已就绪/);
        assert.match(await page(".compile-image"), /随 GizMoth 提供/);
        assert.match(await page(".compile-packages-card"), /linux-arm64/);
        assert.equal(await app.browser.$("button*=添加").isExisting(), false, "image packages are fixed");
        await app.screenshot("compile-settings");

        // Settings › Storage: moving the working directory carries the slots,
        // caches, artifacts and packages, and the next build runs from there.
        await app.browser.$('button[data-page="compile-caches"]').click();
        await app.browser.$(".compile-storage-link").click();
        // Moves wait for package downloads and packing.
        await waitFor(async () => !(await snapshot()).packages.busy, "packages idle", 180000);
        await app.browser.$(".storage-tree").waitForDisplayed();
        const before = (await snapshot()).storage;
        assert.equal(before.isDefault, true);
        assert.match(await page(".storage-tree-root"), /默认/);
        // One row per kind of data below the working directory.
        assert.equal((await app.browser.$$(".storage-tree-item")).length, 5);
        const working = join(app.temporary, "working");
        const moved = await app.invoke("compile_request", {
          method: "POST",
          path: "/v1/compile/storage",
          body: { path: working },
        });
        assert.equal(moved.ok, true, JSON.stringify(moved));
        await waitFor(async () => (await snapshot()).storage.moving === null, "working directory moved", 120000);
        const after = await snapshot();
        assert.equal(after.storage.error, null);
        assert.equal(after.storage.isDefault, false);
        assert.equal(after.artifacts.dir, join(after.storage.root, "artifacts"));
        await waitFor(async () => /自定义/.test(await page(".storage-tree-root")), "new location in the page");
        assert.match(await page(".storage-tree-root"), /working/);
        for (const item of after.storage.items) assert.ok(item.path.startsWith(after.storage.root), item.path);
        assert.equal(after.storage.items.find((item) => item.id === "caches").exists, true);
        assert.equal(
          await app.browser.$('[data-storage-item="caches"] .storage-tree-actions button').isEnabled(),
          true,
        );
        await app.screenshot("settings-storage");
        const files = await app.invoke("compile_request", {
          method: "GET",
          path: `/v1/compile/artifacts/${artifact.id}`,
          body: null,
        });
        assert.equal(files.ok, true);
        assert.ok(files.value.files[0].absolutePath.startsWith(after.storage.root), files.value.files[0].absolutePath);
        const again = await build(info, { workspace, command: ["make", "-B"] }, "E2E after move");
        assert.equal(again.finished?.job.status, "succeeded", again.log);
      } finally {
        await stopService(app.data);
      }
    }),
);
