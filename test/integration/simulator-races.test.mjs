import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import test from "node:test";
import { chooseSelect, runNativeAsync, waitFor, withApp } from "./native-app.mjs";

async function preview(app, kind) {
  const profile = { id: randomUUID(), name: `Race ${kind}`, model: "h106-tiga", firmwareId: "", view: "screen" };
  assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
  await app.browser.$('button[data-page="simulators"]').click();
  await app.browser.$(`button[aria-label="打开 ${profile.name}"]`).click();
  const label = `simulator-${profile.id.replaceAll("-", "")}`;
  await waitFor(async () => (await app.browser.getWindowHandles()).includes(label), "preview window opens");
  await app.browser.switchToWindow(label);
  await app.browser.$(".game-preview").waitForDisplayed();
  await waitFor(
    () =>
      runNativeAsync(
        app.browser,
        async (kind, done) => {
          const host = document.querySelector(".game-preview");
          let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
          let player;
          for (; fiber && !player; fiber = fiber.return) {
            for (let hook = fiber.memoizedState; hook; hook = hook.next) {
              if (hook.memoizedState?.current?.executeCommand) player = hook.memoizedState.current;
            }
          }
          if (!player) return done(false);
          await player.ready;
          const initial = player.state;
          const probe = (window.__simulatorRace = {
            entered: false,
            released: false,
            starts: 0,
            commands: 0,
            mode: "preview",
            phase: "idle",
            release: null,
          });
          // Delay admission before real firmware/media side effects. The host's request,
          // cancellation, SDK controller identity and native window transport stay real.
          Object.defineProperty(player, "state", {
            configurable: true,
            get: () => ({ ...initial, mode: probe.mode, phase: probe.phase }),
          });
          player.enter = async () => {
            probe.entered = true;
            probe.mode = "play";
            probe.phase = "installing";
            await new Promise((resolve) => {
              probe.release = resolve;
            });
            probe.phase = "ready";
            probe.released = true;
          };
          player.start = () => {
            probe.starts++;
            probe.phase = "running";
          };
          player.executeCommand = async () => {
            probe.commands++;
            throw new Error("Unexpected command after cancellation");
          };
          done(true);
        },
        kind,
      ),
    "preview controller is instrumented",
    45000,
  );
  await app.browser.switchToWindow("main");
  await app.browser.$('button[data-page="projects"]').click();
  await app.openProject("alpha");
  await app.idle();
  await app.browser.$('button[aria-label="项目信息"]').click();
  await chooseSelect(app, "设备与模拟器运行", "allow");
  await app.browser.$('button[aria-label="关闭项目信息"]').click();
  return { profile, label };
}

function queueRun(app, profileId) {
  const request = app.model.enqueue({
    tools: [{ id: "open-race", name: "run_in_simulator", arguments: { profileId } }],
  });
  const response = app.model.enqueue({ text: "Run request observed." });
  return { request, response };
}

async function finish(app, response) {
  await waitFor(() => !!response.request, "tool result reaches the model", 30000);
  await app.idle();
}

const options = { timeout: 90000 };
test("revoking run access cancels a pending assistant simulator startup", options, (t) =>
  withApp(t, async (app) => {
    const { profile, label } = await preview(app, "revocation");
    const { response } = queueRun(app, profile.id);
    await app.send("Start this local simulator");
    await finish(app, response);
    await app.browser.switchToWindow(label);
    await waitFor(() => app.browser.execute(() => window.__simulatorRace.entered), "startup awaits installation");
    await app.browser.switchToWindow("main");
    await app.browser.$('button[aria-label="项目信息"]').click();
    await chooseSelect(app, "设备与模拟器运行", "deny");
    await app.browser.switchToWindow(label);
    await waitFor(
      async () => !(await app.browser.$(".preview-incoming-app").isExisting()),
      "revocation clears the queued project",
    );
    await app.browser.execute(() => window.__simulatorRace.release());
    await waitFor(
      () => app.browser.execute(() => window.__simulatorRace.released),
      "installation result settles after revocation",
    );
    // Let promise continuations drain, then inspect the actual controller calls.
    const result = await runNativeAsync(app.browser, (done) =>
      setTimeout(() => done({ starts: window.__simulatorRace.starts, commands: window.__simulatorRace.commands }), 100),
    );
    assert.deepEqual(
      result,
      { starts: 0, commands: 0 },
      "revoked requests cannot boot or install/run Lua after the waiter settles",
    );
    await app.browser.switchToWindow("main");
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("late simulator opening cannot override a newer physical-device choice", options, (t) =>
  withApp(
    t,
    async (app) => {
      const { profile, label } = await preview(app, "target-selection");
      await app.browser.execute(() => {
        const callbacks = window.__TAURI_INTERNALS__.callbacks;
        const set = callbacks.set;
        const probe = (window.__targetSelectionRace = {
          held: false,
          release: null,
          restore: () => {
            callbacks.set = set;
          },
        });
        let armed = true;
        callbacks.set = (id, callback) =>
          set.call(callbacks, id, (value) => {
            if (armed && typeof value === "string" && value.startsWith("simulator-")) {
              armed = false;
              probe.held = true;
              probe.release = () => callback(value);
            } else callback(value);
          });
      });
      const { response } = queueRun(app, profile.id);
      await app.send("Open this simulator while I choose a different run target");
      await waitFor(
        () => app.browser.execute(() => window.__targetSelectionRace.held),
        "native opening response is held",
      );
      await app.browser.$('button[aria-label="运行"]').click();
      const targets = await app.browser.$$(".run-target-row");
      const names = await targets.map((target) => target.getText());
      const index = names.findIndex((name) => name.includes("Synthetic physical device"));
      assert.ok(index >= 0);
      await targets[index].click();
      await app.browser.execute(() => window.__targetSelectionRace.release());
      await finish(app, response);
      await app.browser.execute(() => window.__targetSelectionRace.restore());
      const check = app.model.enqueue({
        tools: [
          { id: "selected", name: "list_simulators", arguments: {} },
          { id: "hardware", name: "push_to_device", arguments: {} },
        ],
      });
      const checked = app.model.enqueue({ text: "Target selection checked." });
      await app.send("Inspect the target and retry the legacy run tool");
      await waitFor(() => !!check.request, "target check starts");
      await finish(app, checked);
      const results = Object.fromEntries(
        checked.request.body.messages
          .filter((message) => message.role === "tool")
          .map((message) => [message.tool_call_id, message.content]),
      );
      assert.equal(JSON.parse(results.selected).selected, null, "the newer hardware selection stays current");
      assert.match(
        results.hardware,
        /disabled in native integration tests/,
        "the hardware branch reaches the fixture's IPC guard rather than retargeting the simulator",
      );
      await app.browser.switchToWindow(label);
      await app.browser.execute(() => window.__simulatorRace.release?.());
      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
    },
    {
      devices: [
        { id: "physical-fixture", name: "Synthetic physical device", baseUrl: "https://device.example.invalid" },
      ],
    },
  ),
);
