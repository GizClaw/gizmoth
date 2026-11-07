import test from "node:test";
import assert from "node:assert/strict";
import {
  createUpdater,
  downloadPercent,
  type UpdateDriver,
  type UpdateEvent,
  type UpdateResource,
  type UpdateState,
  type UpdateCheckState,
} from "../src/updater.ts";

function fixture() {
  const calls = { downloads: 0, installs: 0, restarts: 0, closes: 0 };
  let progress: (event: UpdateEvent) => void = () => {};
  let finish: () => void = () => {};
  let reject: (error: Error) => void = () => {};
  const resource: UpdateResource = {
    version: "1.2.0",
    download(onEvent) {
      calls.downloads++;
      progress = onEvent;
      return new Promise<void>((resolve, fail) => {
        finish = resolve;
        reject = fail;
      });
    },
    async install() {
      calls.installs++;
    },
    async close() {
      calls.closes++;
    },
  };
  const driver: UpdateDriver = {
    check: async () => resource,
    restart: async () => {
      calls.restarts++;
    },
  };
  return {
    calls,
    driver,
    resource,
    progress: (event: UpdateEvent) => progress(event),
    finish: () => finish(),
    reject: () => reject(new Error("Invalid update signature")),
  };
}

test("no release and offline checks leave the titlebar hidden", async () => {
  const controller = createUpdater({ check: async () => null, restart: async () => {} }, () => {});
  await controller.check();
  assert.deepEqual(controller.state, { phase: "hidden" });
  const offline = createUpdater(
    {
      check: async () => {
        throw new Error("404");
      },
      restart: async () => {},
    },
    () => {},
  );
  await offline.check();
  assert.deepEqual(offline.state, { phase: "hidden" });
});

test("manual checks report current, failed and available outcomes without inventing an update", async () => {
  const fake = fixture();
  let outcome: "current" | "error" | "available" = "current";
  fake.driver.check = async () => {
    if (outcome === "error") throw new Error("Offline");
    return outcome === "available" ? fake.resource : null;
  };
  const checks: UpdateCheckState[] = [];
  const controller = createUpdater(
    fake.driver,
    () => {},
    async () => {},
    (state) => checks.push(state),
  );
  await controller.check();
  assert.deepEqual(checks, [{ phase: "checking" }, { phase: "current" }]);
  outcome = "error";
  await controller.check();
  assert.deepEqual(checks.at(-1), { phase: "error", message: "Offline" });
  assert.equal(controller.state.phase, "hidden");
  outcome = "available";
  await controller.check();
  assert.deepEqual(checks.at(-1), { phase: "available" });
  assert.equal(controller.state.phase, "available");
  assert.equal(fake.calls.downloads, 0);
  controller.dispose();
});

test("automatic and manual checks share an in-flight request", async () => {
  let calls = 0;
  let finish: (value: UpdateResource | null) => void = () => {};
  const controller = createUpdater(
    {
      check: () => {
        calls++;
        return new Promise((resolve) => {
          finish = resolve;
        });
      },
      restart: async () => {},
    },
    () => {},
  );
  const first = controller.check();
  await controller.check();
  assert.equal(calls, 1);
  finish(null);
  await first;
  assert.equal(controller.state.phase, "hidden");
  controller.dispose();
});

test("one click downloads, verifies, saves, installs and restarts in order", async () => {
  const fake = fixture();
  const states: UpdateState[] = [];
  let preparations = 0;
  const controller = createUpdater(
    fake.driver,
    (state) => states.push(state),
    async () => {
      preparations++;
      assert.equal(fake.calls.installs, 0);
    },
  );
  await controller.check();
  const downloading = controller.activate();
  await controller.activate();
  fake.progress({ event: "Started", data: { contentLength: 1000 } });
  fake.progress({ event: "Progress", data: { chunkLength: 420 } });
  assert.equal(downloadPercent(controller.state), 42);
  fake.progress({ event: "Progress", data: { chunkLength: 580 } });
  assert.equal(downloadPercent(controller.state), 99);
  fake.progress({ event: "Finished" });
  assert.equal(controller.state.phase, "verifying");
  assert.equal(fake.calls.installs, 0);
  fake.finish();
  await downloading;
  assert.equal(fake.calls.downloads, 1);
  assert.equal(preparations, 1);
  assert.equal(fake.calls.installs, 1);
  assert.ok(
    states.findIndex((state) => state.phase === "verifying") <
      states.findIndex((state) => state.phase === "installing"),
  );
  assert.equal(fake.calls.restarts, 1);
  assert.equal(controller.state.phase, "installed");
  assert.equal(fake.calls.closes, 1);
  fake.driver.check = async () => null;
  await controller.check();
  assert.equal(controller.state.phase, "hidden");
  controller.dispose();
});

test("missing Content-Length remains indeterminate and bad signatures never install", async () => {
  const fake = fixture();
  const controller = createUpdater(fake.driver, () => {});
  await controller.check();
  const work = controller.activate();
  fake.progress({ event: "Started", data: {} });
  fake.progress({ event: "Progress", data: { chunkLength: 1024 } });
  assert.equal(downloadPercent(controller.state), undefined);
  fake.progress({ event: "Finished" });
  fake.reject();
  await work;
  assert.equal(controller.state.phase, "error");
  assert.equal(fake.calls.installs, 0);
  const retry = controller.activate();
  assert.equal(fake.calls.downloads, 2);
  fake.finish();
  await retry;
  assert.equal(controller.state.phase, "installed");
  assert.equal(fake.calls.installs, 1);
  assert.equal(fake.calls.restarts, 1);
  controller.dispose();
});

test("project save failure prevents installation and retry reuses the verified download", async () => {
  const fake = fixture();
  let blocked = true;
  const controller = createUpdater(
    fake.driver,
    () => {},
    async () => {
      if (blocked) throw new Error("Unsaved file");
    },
  );
  await controller.check();
  const download = controller.activate();
  fake.finish();
  await download;
  assert.equal(controller.state.phase, "error");
  assert.equal(controller.state.phase === "error" && controller.state.retry, "install");
  assert.equal(fake.calls.installs, 0);
  blocked = false;
  await controller.activate();
  assert.equal(fake.calls.downloads, 1);
  assert.equal(fake.calls.installs, 1);
  assert.equal(fake.calls.restarts, 1);
  controller.dispose();
});

test("failed restart retries only restart, not installation", async () => {
  const fake = fixture();
  let blocked = true;
  fake.driver.restart = async () => {
    fake.calls.restarts++;
    if (blocked) throw new Error("Restart refused");
  };
  const controller = createUpdater(fake.driver, () => {});
  await controller.check();
  const work = controller.activate();
  fake.finish();
  await work;
  assert.equal(controller.state.phase, "error");
  assert.equal(controller.state.phase === "error" && controller.state.retry, "restart");
  blocked = false;
  await controller.activate();
  assert.equal(fake.calls.installs, 1);
  assert.equal(fake.calls.restarts, 2);
  controller.dispose();
});

test("late check results and in-flight downloads are closed after unmount", async () => {
  const fake = fixture();
  let found: (value: UpdateResource) => void = () => {};
  fake.driver.check = () =>
    new Promise((resolve) => {
      found = resolve;
    });
  let changes = 0;
  const controller = createUpdater(fake.driver, () => {
    changes++;
  });
  const checking = controller.check();
  controller.dispose();
  found(fake.resource);
  await checking;
  assert.equal(fake.calls.closes, 1);
  assert.equal(changes, 0);
  const other = fixture();
  const active = createUpdater(other.driver, () => {});
  await active.check();
  const downloading = active.activate();
  active.dispose();
  other.finish();
  await downloading;
  assert.equal(other.calls.closes, 1);
  assert.equal(other.calls.installs, 0);
});
