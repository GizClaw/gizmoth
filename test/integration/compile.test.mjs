import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { lstat, mkdir, readFile, readdir, readlink, symlink, writeFile } from "node:fs/promises";
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
// `npm run linux-kernel:stage` staged. Without GIZMOTH_E2E_COMPILE_SEED (another
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
          for (const item of ["assets", "registry", "packages.json", "packages.erofs", "packages.erofs.json"]) {
            // APFS clones cost no extra space.
            await exec("cp", ["-cR", join(seed, item), join(app.data, "compile", item)]).catch(() => {});
          }
        }
        const workspace = join(app.temporary, "workspace");
        await mkdir(join(workspace, "src"), { recursive: true });
        await mkdir(join(workspace, "task-cache"));
        await writeFile(join(workspace, "task-cache/ignored.txt"), "excluded from this build copy\n");
        await writeFile(
          join(workspace, "src/main.c"),
          '#include <stdio.h>\nint main(void){puts("native e2e");return 0;}\n',
        );
        await symlink("main.c", join(workspace, "src/entry.c"));
        await symlink("src", join(workspace, "source-dir-link"));
        await symlink("missing-target", join(workspace, "dangling-source-link"));
        await writeFile(
          join(workspace, "Makefile"),
          "out/app.update.tar.zlib: src/main.c src/entry.c\n\tmkdir -p out\n\tgcc -O2 -o out/app src/entry.c\n\tcp out/app $@\n",
        );
        const snapshot = async () => {
          const result = await app.invoke("compile_request", { method: "GET", path: "/v1/compile", body: null });
          assert.equal(result.ok, true, JSON.stringify(result));
          return result.value;
        };
        const page = (selector) =>
          app.browser.execute((selector) => document.querySelector(selector)?.textContent ?? "", selector);
        const assertHostSlots = async (state) => {
          for (const slot of state.slots) {
            assert.equal("diskGiB" in slot, false);
            assert.equal("diskGib" in slot, false);
            assert.equal(slot.disk.path, join(state.storage.root, "slots", slot.id, "work"));
            assert.equal(slot.disk.size, null);
            assert.equal(slot.disk.free, null);
            await assert.rejects(lstat(join(state.storage.root, "slots", slot.id, "work.img")), { code: "ENOENT" });
          }
          for (const slot of state.settings.slots) assert.deepEqual(Object.keys(slot).sort(), ["id", "name"]);
        };
        const sourceLinks = [
          'test -L src/entry.c && test "$(readlink src/entry.c)" = main.c',
          'test -L source-dir-link && test "$(readlink source-dir-link)" = src',
          'test -L dangling-source-link && test "$(readlink dangling-source-link)" = missing-target',
          "test ! -e dangling-source-link && test -f source-dir-link/main.c",
          "test -L /src/src/entry.c && test -L /src/dangling-source-link",
        ].join(" && ");

        // Opening the page starts the fixture's service and reads it live.
        await app.browser.$('button[data-page="compile-builds"]').click();
        await app.browser.$(".compile-slots").waitForDisplayed({ timeout: 60000 });
        const initial = await snapshot();
        assert.equal(initial.available.ok, true, initial.available.message);
        assert.equal(initial.image.status, "ready", "bundled boot components are internal app resources");
        assert.equal(initial.slots.length, 1);
        await assertHostSlots(initial);
        assert.equal(await app.browser.$(".compile-unavailable").isExisting(), false);
        assert.match(await page(".compile-slots"), /直接使用宿主工作目录的可用空间/);
        // Views are second-level sidebar entries under 编译, not page tabs.
        assert.equal((await app.browser.$$("#nav-compiling .nav-child")).length, 3);
        assert.equal(await app.browser.$('[role="tablist"]').isExisting(), false);

        // Add a second slot from the page.
        await app.browser.$("button*=添加 slot").click();
        const name = await app.browser.$('input[id$="-name"]');
        await name.waitForDisplayed();
        const slotDialog = await app.browser.$("dialog[open]");
        assert.equal((await slotDialog.$$("form input")).length, 1, "slot creation asks only for a name");
        assert.equal((await slotDialog.$$('input[type="number"]')).length, 0, "no per-slot disk or resource controls");
        await name.setValue("E2E Slot");
        await app.browser.$('button[type="submit"]').click();
        await waitFor(async () => (await snapshot()).slots.length === 2, "slot added through the page");
        await waitFor(async () => (await app.browser.$$(".compile-slot")).length === 2, "second slot card", 20000);
        assert.match(await page(".compile-slots"), /E2E Slot/);
        await assertHostSlots(await snapshot());

        // A real build submitted by an agent appears live, then finishes with an artifact.
        const info = JSON.parse(await readFile(join(app.data, "serial", "daemon.json"), "utf8"));
        const make = build(
          info,
          {
            workspace,
            command: [
              "bash",
              "-lc",
              `${sourceLinks} && python3 -I -c 'import os; f=[os.open("/dev/null",os.O_RDONLY) for _ in range(8193)]; [os.close(x) for x in f]' && make`,
            ],
            artifacts: ["out/*.update.tar.zlib"],
          },
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
        const built = await snapshot();
        await assertHostSlots(built);
        const hostWork = built.slots.find((slot) => slot.id === result.finished.job.slot).disk.path;
        assert.equal((await lstat(hostWork)).isDirectory(), true);
        const copies = (await readdir(join(hostWork, "src"), { withFileTypes: true })).filter(
          (entry) => entry.isDirectory() && entry.name !== ".manifests",
        );
        assert.equal(copies.length, 1, "one host-backed copy for this workspace");
        const buildCopy = join(hostWork, "src", copies[0].name);
        assert.equal(await readlink(join(buildCopy, "src/entry.c")), "main.c");
        assert.equal(await readlink(join(buildCopy, "source-dir-link")), "src");
        assert.equal(await readlink(join(buildCopy, "dangling-source-link")), "missing-target");
        const artifact = result.finished.artifact;
        assert.deepEqual(
          artifact.files.map((file) => file.path),
          ["out/app.update.tar.zlib"],
        );
        assert.deepEqual(
          await readFile(join(buildCopy, "out/app.update.tar.zlib")),
          await readFile(artifact.files[0].absolutePath),
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

        // Reuse the warm VM and restore inputs modified or deleted by its build.
        const edited = await build(
          info,
          {
            workspace,
            command: [
              "bash",
              "-lc",
              "printf broken > src/main.c; rm Makefile src/entry.c; ln -s missing.c src/entry.c",
            ],
          },
          "E2E guest source edits",
        );
        assert.equal(edited.finished?.job.status, "succeeded", edited.log);
        assert.equal(
          edited.finished.job.slot,
          result.finished.job.slot,
          "the warm slot retains its host work directory",
        );
        assert.equal(await readlink(join(buildCopy, "src/entry.c")), "missing.c");
        assert.equal(await readlink(join(workspace, "src/entry.c")), "main.c");
        assert.match(await readFile(join(workspace, "src/main.c"), "utf8"), /native e2e/);
        const restored = await build(
          info,
          {
            workspace,
            command: [
              "bash",
              "-lc",
              `grep -q 'native e2e' src/main.c && test -f Makefile && test -f /src/src/main.c && ${sourceLinks} && ! touch /src/.gizmoth-write-proof 2>/dev/null`,
            ],
          },
          "E2E restored sources",
        );
        assert.equal(restored.finished?.job.status, "succeeded", restored.log);
        assert.equal(restored.finished.job.slot, edited.finished.job.slot, "reused the same slot's workspace");
        assert.equal(await readlink(join(buildCopy, "src/entry.c")), "main.c");

        // Change cache policy while busy; the next VM must receive the new internal configuration.
        const nextCaches = structuredClone((await snapshot()).settings.caches);
        nextCaches.ccache.localGib += 1;
        let settingsPatch;
        const busy = await build(
          info,
          { workspace, command: ["bash", "-lc", "sleep 2"] },
          "E2E boot settings while busy",
          {
            closeWhen: (frame) => {
              if (frame.type === "status" && frame.status === "running" && !settingsPatch) {
                settingsPatch = app.invoke("compile_request", {
                  method: "PATCH",
                  path: "/v1/compile/settings",
                  body: { caches: nextCaches },
                });
              }
              return false;
            },
          },
        );
        assert.equal(busy.finished?.job.status, "succeeded", busy.log);
        assert.ok(settingsPatch, "settings changed while the build was running");
        const patched = await settingsPatch;
        assert.equal(patched.ok, true, JSON.stringify(patched));
        const updated = await build(
          info,
          {
            workspace,
            env: { GIZMOTH_E2E_REQUEST: "task" },
            exclude: ["task-cache/**"],
            command: [
              "bash",
              "-lc",
              `test "$GIZMOTH_E2E_REQUEST" = task && test -e /sys/class/net/eth0 && grep -q '^max_size = ${nextCaches.ccache.localGib}G$' /etc/ccache.conf && test ! -e task-cache/ignored.txt && grep -q 'native e2e' src/main.c && test -f Makefile`,
            ],
          },
          "E2E updated VM policy and task environment",
        );
        assert.equal(updated.finished?.job.status, "succeeded", updated.log);
        const isolated = await build(
          info,
          { workspace, command: ["bash", "-lc", 'test -z "${GIZMOTH_E2E_REQUEST+x}"'] },
          "E2E task environment isolation",
        );
        assert.equal(isolated.finished?.job.status, "succeeded", isolated.log);

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

        // A modal makes the sidebar inert until it is explicitly closed.
        await dialog.$('button[aria-label="关闭"]').click();
        await dialog.waitForDisplayed({ reverse: true });

        // Caches: Bazel and ccache rows from the host cache store.
        await app.browser.$('button[data-page="compile-caches"]').click();
        await waitFor(async () => /ccache/.test(await page(".compile-panel")), "cache rows");
        await app.screenshot("compile-caches");

        // Settings › 编译 exposes resource policy and managed downloads, not internal boot components.
        await app.browser.$('button[data-page="settings"]').click();
        await app.browser.$('[data-settings-section="compiling"]').click();
        await app.browser.$(".compile-settings").waitForDisplayed();
        assert.equal(await app.browser.$(".compile-image").isExisting(), false);
        assert.equal(await app.browser.$(".compile-page .page-description").isExisting(), false);
        assert.doesNotMatch(await page(".compile-settings"), /Linux 启动环境|内核与静态 agent/);
        assert.match(await page(".compile-packages-card"), /linux-arm64/);
        assert.doesNotMatch(await page(".compile-packages-card"), /清理下载归档/);
        assert.equal(await app.browser.$("button*=添加").isExisting(), false, "tool selection is automatic");
        assert.match(await page(".compile-settings"), /所有构建与准备 VM 共享的总预算/);
        assert.equal(await app.browser.$(".compile-settings > section:first-child h2").getText(), "虚拟机");
        assert.equal((await app.browser.$$(".compile-settings textarea")).length, 0);
        for (const key of ["bazelrc", "env", "excludes", "network"])
          assert.equal(key in (await snapshot()).settings, false);
        assert.equal(await app.browser.$('input[aria-label="虚拟机联网"]').isExisting(), false);
        assert.equal((await app.browser.$$(".compile-vm-list > .settings-row")).length, 5);
        assert.equal((await app.browser.$$(".compile-vm-list .compile-inset-unit")).length, 3);
        const autoStop = await app.browser.$('input[aria-label="自动停止空闲 VM"]');
        const idleDelay = await app.browser.$('input[aria-label="空闲停止等待时间"]');
        await autoStop.click();
        assert.equal(await idleDelay.isEnabled(), false);
        const memory = await app.browser.$('input[aria-label="全部 VM 内存总预算"]');
        const cpu = await app.browser.$('input[aria-label="全部 VM 虚拟核总预算"]');
        await memory.scrollIntoView();
        await app.browser.$("button=停止空闲 VM").click();
        await waitFor(async () => (await snapshot()).resources.vmCount === 0, "idle VM reservations released", 60000);
        await memory.setValue("4");
        await cpu.setValue("4");
        await app.browser.$('button[type="submit"]').click();
        await waitFor(async () => (await snapshot()).settings.vmResources.memoryMiB === 4096, "global VM budget saved");
        const resources = (await snapshot()).resources;
        assert.deepEqual(resources.limit, { cpus: 4, memoryMiB: 4096 });
        assert.deepEqual(resources.allocated, { cpus: 0, memoryMiB: 0 });
        assert.equal((await snapshot()).settings.autoStopIdle, false);
        await autoStop.click();
        assert.equal(await idleDelay.isEnabled(), true);
        await app.browser.$('button[type="submit"]').click();
        await waitFor(async () => (await snapshot()).settings.autoStopIdle, "auto idle stop re-enabled");
        await app.browser.execute(() =>
          document
            .querySelector('input[aria-label="全部 VM 内存总预算"]')
            .closest("section")
            .scrollIntoView({ block: "start" }),
        );
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
        assert.equal(await app.browser.$('button[aria-label="清理下载归档"]').isExisting(), true);
        assert.equal(await app.browser.$('button[aria-label="清理产物"]').isExisting(), true);
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
        await assertHostSlots(after);
        const movedHostWork = after.slots.find((slot) => slot.id === result.finished.job.slot).disk.path;
        assert.equal((await lstat(movedHostWork)).isDirectory(), true);
        assert.equal(await readlink(join(movedHostWork, "src", copies[0].name, "src/entry.c")), "main.c");
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
