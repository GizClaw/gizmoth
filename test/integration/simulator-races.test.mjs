import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import test from "node:test";
import { chooseSelect, runNativeAsync, waitFor, withApp } from "./native-app.mjs";

async function preview(app, kind, holdInspect = false) {
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
        async (holdInspect, done) => {
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
            holdInspect,
            inspections: [],
            appReplies: [],
          });
          const callbacks = window.__TAURI_INTERNALS__.callbacks;
          const set = callbacks.set;
          const wrap = (callback) => (value) => {
            const payload = value?.payload;
            if (payload?.type === "gizmoth-simulator-reply") probe.appReplies.push(payload.app?.id ?? null);
            if (
              probe.holdInspect &&
              payload?.type === "gizmoth-simulator-control" &&
              payload.command.action === "inspect"
            ) {
              probe.inspections.push(payload.command.requestId);
              return;
            }
            callback(value);
          };
          for (const [id, callback] of callbacks) set.call(callbacks, id, wrap(callback));
          callbacks.set = (id, callback) => set.call(callbacks, id, wrap(callback));
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
        holdInspect,
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

function queueRun(app, profileId, followup = true) {
  const request = app.model.enqueue({
    tools: [{ id: `open-race-${randomUUID()}`, name: "run_in_simulator", arguments: { profileId } }],
  });
  const response = followup ? app.model.enqueue({ text: "Run request observed." }) : undefined;
  return { request, response };
}

async function finish(app, response) {
  await waitFor(() => !!response.request, "tool result reaches the model", 30000);
  await app.idle();
}

async function holdOpening(app) {
  await app.browser.execute(() => {
    const callbacks = window.__TAURI_INTERNALS__.callbacks;
    const set = callbacks.set;
    const probe = (window.__toolOpeningRace = { held: false, release: null });
    let armed = true;
    callbacks.set = (id, callback) =>
      set.call(callbacks, id, (value) => {
        if (armed && typeof value === "string" && value.startsWith("simulator-")) {
          armed = false;
          probe.held = true;
          probe.release = () => {
            callbacks.set = set;
            callback(value);
          };
        } else callback(value);
      });
  });
}

async function userRun(app, profile) {
  await app.browser.switchToWindow("main");
  await app.browser.$('button[aria-label="运行"]').click();
  let target;
  await waitFor(async () => {
    const targets = await app.browser.$$(".run-target-row");
    const names = await targets.map((row) => row.getText());
    target = targets[names.findIndex((name) => name.includes(profile.name))];
    return !!target;
  }, "the explicit UI Run loads the simulator target");
  await target.click();
}

async function stop(app) {
  await app.browser.switchToWindow("main");
  await app.browser.$("aside.agent .agent-stop-hint").waitForDisplayed();
  await app.browser.keys("Escape");
}

const options = { timeout: 90000 };

test("stopping an assistant run cancels its pending opening and startup", options, (t) =>
  withApp(t, async (app) => {
    for (const boundary of ["opening", "startup", "response"]) {
      const { profile, label } = await preview(app, `stop-${boundary}`, boundary === "startup");
      if (boundary === "opening") await holdOpening(app);
      const previousRequests = app.model.requests.length;
      queueRun(app, profile.id, false);
      const held =
        boundary === "response" ? app.model.enqueue({ hold: true, thinking: "Simulator startup is queued." }) : null;
      await app.send("Start this simulator until I stop the assistant");
      if (boundary === "opening") {
        await waitFor(() => app.browser.execute(() => window.__toolOpeningRace.held), "opening response is held");
      } else if (held) {
        await waitFor(() => !!held.request, "the tool returned while startup is still pending");
        await app.busy();
      } else {
        await app.browser.switchToWindow(label);
        await waitFor(
          () =>
            app.browser.execute(() => window.__simulatorRace.entered && window.__simulatorRace.inspections.length > 0),
          "startup and readiness inspection are pending",
        );
      }
      await stop(app);
      if (boundary === "opening") await app.browser.execute(() => window.__toolOpeningRace.release());
      await app.idle();
      held?.release();
      assert.equal(
        app.model.requests.length,
        previousRequests + (held ? 2 : 1),
        "Stop does not continue to another model request",
      );
      await app.browser.switchToWindow(label);
      await waitFor(
        () => app.browser.execute(() => window.__simulatorRace.appReplies.length > 0),
        "the child observes the current app request",
      );
      assert.equal(
        await app.browser.$(".preview-incoming-app").isExisting(),
        false,
        "Stop clears the assistant request",
      );
      await app.browser.execute(() => window.__simulatorRace.release?.());
      const observed = await runNativeAsync(app.browser, (done) =>
        setTimeout(
          () => done({ starts: window.__simulatorRace.starts, commands: window.__simulatorRace.commands }),
          100,
        ),
      );
      assert.deepEqual(observed, { starts: 0, commands: 0 }, "a stopped tool cannot boot or launch its queued app");
      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
    }
  }),
);

test("stopping an older assistant run preserves a newer explicit UI Run", options, (t) =>
  withApp(t, async (app) => {
    const { profile, label } = await preview(app, "stop-with-successor");
    await holdOpening(app);
    queueRun(app, profile.id, false);
    await app.send("Open this simulator while I run it myself");
    await waitFor(() => app.browser.execute(() => window.__toolOpeningRace.held), "assistant opening response is held");
    await userRun(app, profile);
    await app.browser.switchToWindow(label);
    await waitFor(
      () =>
        app.browser.execute(() => window.__simulatorRace.entered && window.__simulatorRace.appReplies.some(Boolean)),
      "the successor UI request awaits installation",
    );
    const successor = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
    await stop(app);
    await app.browser.execute(() => window.__toolOpeningRace.release());
    await app.idle();
    await app.browser.switchToWindow(label);
    assert.equal(await app.browser.$(".preview-incoming-app").isExisting(), true, "the UI Run survives assistant Stop");
    assert.equal(await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1)), successor);
    await app.browser.execute(() => window.__simulatorRace.release());
    await waitFor(
      () => app.browser.execute(() => window.__simulatorRace.starts === 1),
      "the UI Run can still power on",
    );
    await app.browser.switchToWindow("main");
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("readiness retries cannot transfer assistant button consent to a newer UI Run", options, (t) =>
  withApp(t, async (app) => {
    const { profile, label } = await preview(app, "consent-successor", true);
    await app.browser.$('button[aria-label="项目信息"]').click();
    await chooseSelect(app, "设备与模拟器运行", "ask");
    await app.browser.$('button[aria-label="关闭项目信息"]').click();
    const { response } = queueRun(app, profile.id);
    await app.send("Approve one simulator test run");
    const confirm = await app.browser.$('//dialog//button[normalize-space()="允许"]');
    await confirm.waitForDisplayed();
    await confirm.click();
    await app.browser.switchToWindow(label);
    await waitFor(
      () => app.browser.execute(() => window.__simulatorRace.inspections.length > 0),
      "the confirmed request is waiting for readiness",
    );
    const original = await app.browser.execute(() => window.__simulatorRace.inspections[0]);
    await userRun(app, profile);
    await app.browser.switchToWindow(label);
    await waitFor(
      () =>
        app.browser.execute(
          (original) => window.__simulatorRace.appReplies.some((id) => id && id !== original),
          original,
        ),
      "a distinct UI Run replaces the confirmed request",
    );
    await app.browser.execute(() => {
      window.__simulatorRace.holdInspect = false;
    });
    await app.browser.switchToWindow("main");
    await finish(app, response);
    const runResults = response.request.body.messages.filter((message) => message.role === "tool");
    assert.match(runResults.at(-1).content, /canceled or superseded/, "readiness rejects the superseded invocation");
    app.model.enqueue({
      tools: [
        { id: "successor-input", name: "press_simulator_button", arguments: { profileId: profile.id, button: "ok" } },
      ],
    });
    const checked = app.model.enqueue({ text: "Successor input consent checked." });
    await app.send("Try a button without approving another assistant run");
    await finish(app, checked);
    const input = checked.request.body.messages.find(
      (message) => message.role === "tool" && message.tool_call_id === "successor-input",
    );
    assert.match(
      input.content,
      /Call run_in_simulator to approve/,
      "the successor never inherits the original confirmation",
    );
    await app.browser.switchToWindow(label);
    assert.equal(
      await app.browser.$(".preview-incoming-app").isExisting(),
      true,
      "supersession preserves the UI request",
    );
    await app.browser.execute(() => window.__simulatorRace.release?.());
    await app.browser.switchToWindow("main");
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);
test("revoking run access cancels a pending assistant simulator startup", options, (t) =>
  withApp(t, async (app) => {
    for (const nextAccess of ["deny", "ask"]) {
      const { profile, label } = await preview(app, `revocation-${nextAccess}`);
      const { response } = queueRun(app, profile.id);
      await app.send("Start this local simulator");
      await finish(app, response);
      await app.browser.switchToWindow(label);
      await waitFor(() => app.browser.execute(() => window.__simulatorRace.entered), "startup awaits installation");
      await app.browser.switchToWindow("main");
      await app.browser.$('button[aria-label="项目信息"]').click();
      await chooseSelect(app, "设备与模拟器运行", nextAccess);
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
        setTimeout(
          () => done({ starts: window.__simulatorRace.starts, commands: window.__simulatorRace.commands }),
          100,
        ),
      );
      assert.deepEqual(
        result,
        { starts: 0, commands: 0 },
        "revoked requests cannot boot or install/run Lua after the waiter settles",
      );
      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
    }
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
