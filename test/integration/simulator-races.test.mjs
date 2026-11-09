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
            buttons: [],
          });
          player.viewer.buttons.subscribe((event) => probe.buttons.push({ ...event, time: Date.now() }));
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
          player.executeCommand = async (command) => {
            if (probe.readyCommands && command.method === "tools.list")
              return [{ name: "lua.app.install" }, { name: "lua.app.run" }];
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

async function holdRunBoundary(app, boundary = "opening") {
  await app.browser.execute((boundary) => {
    const callbacks = window.__TAURI_INTERNALS__.callbacks;
    const set = callbacks.set;
    const probe = (window.__toolOpeningRace = { held: false, release: null });
    let armed = true;
    const matches = (value) =>
      boundary === "project_check"
        ? value?.appId === "alpha" && typeof value?.bytes === "number"
        : boundary === "projects_list"
          ? Array.isArray(value) && value.some((item) => item.id === "alpha" && item.appId === "alpha")
          : boundary === "profiles_list"
            ? Array.isArray(value) && value.some((item) => item.model === "h106-tiga")
            : boundary === "package"
              ? typeof value?.id === "string" && typeof value?.url === "string"
              : typeof value === "string" && value.startsWith("simulator-");
    callbacks.set = (id, callback) =>
      set.call(callbacks, id, (value) => {
        if (armed && matches(value)) {
          armed = false;
          probe.held = true;
          probe.release = () => {
            callbacks.set = set;
            callback(value);
          };
        } else callback(value);
      });
  }, boundary);
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

test("delayed assistant preparation preserves a newer UI Run on the same simulator", options, (t) =>
  withApp(t, async (app) => {
    for (const boundary of ["project_check", "projects_list"]) {
      const { profile, label } = await preview(app, `ui-successor-${boundary}`);
      await holdRunBoundary(app, boundary);
      const { response } = queueRun(app, profile.id);
      await app.send("Prepare a simulator run while I run the project myself");
      await waitFor(() => app.browser.execute(() => window.__toolOpeningRace.held), "assistant preparation is held");
      await userRun(app, profile);
      await app.browser.switchToWindow(label);
      await waitFor(
        () =>
          app.browser.execute(() => window.__simulatorRace.entered && window.__simulatorRace.appReplies.some(Boolean)),
        "the newer UI Run is queued",
      );
      const successor = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
      await app.browser.switchToWindow("main");
      await app.browser.execute(() => window.__toolOpeningRace.release());
      await finish(app, response);
      const result = response.request.body.messages.filter((message) => message.role === "tool").at(-1);
      assert.match(result.content, /superseded/, "the older operation is rejected before replacing the UI request");
      await app.browser.$('button[aria-label="项目信息"]').click();
      await chooseSelect(app, "设备与模拟器运行", "deny");
      await app.browser.switchToWindow(label);
      assert.equal(await app.browser.$(".preview-incoming-app").isExisting(), true);
      assert.equal(await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1)), successor);
      await app.browser.execute(() => window.__simulatorRace.release());
      await waitFor(
        () => app.browser.execute(() => window.__simulatorRace.starts === 1),
        "the preserved UI Run can power on",
      );
      await app.browser.switchToWindow("main");
      await app.browser.$('button[aria-label="关闭项目信息"]').click();
      assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
    }
  }),
);

test("permission tightening releases assistant-held buttons without canceling UI runs", options, (t) =>
  withApp(t, async (app) => {
    for (const access of ["deny", "ask"]) {
      const { profile, label } = await preview(app, `ui-input-${access}`);
      await userRun(app, profile);
      await app.browser.switchToWindow(label);
      await waitFor(() => app.browser.execute(() => window.__simulatorRace.entered), "UI startup is queued");
      const successor = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
      await app.browser.execute(() => {
        window.__simulatorRace.phase = "running";
      });
      await app.browser.switchToWindow("main");
      const id = `ui-key-${access}`;
      app.model.enqueue({
        tools: [
          { id, name: "press_simulator_button", arguments: { profileId: profile.id, button: "ok", durationMs: 3000 } },
        ],
      });
      const response = app.model.enqueue({ text: "Input permission change observed." });
      await app.send("Hold the simulator OK key while I change the permission");
      await app.browser.switchToWindow(label);
      await waitFor(
        () =>
          app.browser.execute(() =>
            window.__simulatorRace.buttons.some((event) => event.button === "ok" && event.pressed),
          ),
        "the real SDK button is held",
      );
      await app.browser.switchToWindow("main");
      await app.browser.$('button[aria-label="项目信息"]').click();
      await chooseSelect(app, "设备与模拟器运行", access);
      await finish(app, response);
      const result = response.request.body.messages.find(
        (message) => message.role === "tool" && message.tool_call_id === id,
      );
      assert.match(
        result.content,
        /disabled or requires renewed approval/,
        "permission changes cancel the active control RPC",
      );
      await app.browser.switchToWindow(label);
      await waitFor(
        () =>
          app.browser.execute(
            () => window.__simulatorRace.buttons.filter((event) => event.button === "ok").at(-1)?.pressed === false,
          ),
        "revocation releases the focus-independent SDK key",
      );
      assert.equal(
        await app.browser.$(".preview-incoming-app").isExisting(),
        true,
        "the UI app request remains active",
      );
      assert.equal(await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1)), successor);
      await app.browser.execute(() => window.__simulatorRace.release?.());
      await app.browser.switchToWindow("main");
      await app.browser.$('button[aria-label="关闭项目信息"]').click();
      assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
    }
  }),
);

test("Stop during simulator profile lookup does not open obsolete consent", options, (t) =>
  withApp(t, async (app) => {
    const profile = {
      id: randomUUID(),
      name: "Lookup cancellation",
      model: "h106-tiga",
      firmwareId: "",
      view: "screen",
    };
    assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
    await app.openProject("alpha");
    await app.idle();
    await holdRunBoundary(app, "profiles_list");
    queueRun(app, profile.id, false);
    await app.send("Prepare a simulator run before asking for consent");
    await waitFor(() => app.browser.execute(() => window.__toolOpeningRace.held), "profile lookup is pending");
    await stop(app);
    await app.browser.execute(() => window.__toolOpeningRace.release());
    await app.idle();
    assert.equal(await app.browser.$("dialog[open]").isExisting(), false, "Stop prevents a late confirmation modal");
    assert.deepEqual(await app.browser.getWindowHandles(), ["main"]);
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("Stop waiting remains canceled after startup or project preparation settles", options, (t) =>
  withApp(t, async (app) => {
    for (const boundary of ["startup", "package"]) {
      const { profile, label } = await preview(app, `canceled-status-${boundary}`);
      if (boundary === "package") {
        await app.browser.switchToWindow(label);
        await app.browser.execute(() => {
          window.__simulatorRace.mode = "play";
          window.__simulatorRace.phase = "running";
          window.__simulatorRace.readyCommands = true;
        });
        await holdRunBoundary(app, "package");
        await app.browser.switchToWindow("main");
      }
      const { response } = queueRun(app, profile.id);
      await app.send("Queue a local run so I can stop waiting");
      await finish(app, response);
      await app.browser.switchToWindow(label);
      await waitFor(
        () =>
          app.browser.execute(
            (boundary) => (boundary === "package" ? window.__toolOpeningRace.held : window.__simulatorRace.entered),
            boundary,
          ),
        `${boundary} is pending`,
      );
      const requestId = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
      await app.browser.$("button=停止等待").click();
      await waitFor(
        async () => /已停止等待/.test(await app.browser.$(".preview-notes").getText()),
        "the cancellation notice is shown",
      );
      await app.browser.$("button=关闭提示").click();
      if (boundary === "package") await app.browser.execute(() => window.__toolOpeningRace.release());
      await app.browser.$(".simulator-delete-text").waitForEnabled();
      await app.browser.switchToWindow("main");
      const id = `canceled-status-${boundary}`;
      app.model.enqueue({ tools: [{ id, name: "inspect_simulator", arguments: { profileId: profile.id } }] });
      const checked = app.model.enqueue({ text: "Canceled status observed." });
      await app.send("Inspect the settled request after its cancellation notice was dismissed");
      await finish(app, checked);
      const message = checked.request.body.messages.find(
        (message) => message.role === "tool" && message.tool_call_id === id,
      );
      const status = JSON.parse(message.content);
      assert.equal(status.requestId, requestId);
      assert.equal(status.app.status, "canceled", `${boundary} records a terminal cancellation outcome`);
      assert.equal(status.app.error, undefined);
      await app.browser.switchToWindow(label);
      await app.browser.execute(() => window.__simulatorRace.release?.());
      const observed = await runNativeAsync(app.browser, (done) =>
        setTimeout(
          () => done({ starts: window.__simulatorRace.starts, commands: window.__simulatorRace.commands }),
          100,
        ),
      );
      assert.deepEqual(
        observed,
        { starts: 0, commands: 0 },
        "settling canceled preparation cannot boot, install or run Lua",
      );
      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
    }
  }),
);

test("stopping an assistant run cancels its pending opening and startup", options, (t) =>
  withApp(t, async (app) => {
    for (const boundary of ["opening", "startup", "response"]) {
      const { profile, label } = await preview(app, `stop-${boundary}`, boundary === "startup");
      if (boundary === "opening") await holdRunBoundary(app);
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
    await holdRunBoundary(app);
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

test("simulator preparation and opening preserve newer physical-device choices", options, (t) =>
  withApp(
    t,
    async (app) => {
      for (const boundary of ["project_check", "projects_list", "opening"]) {
        const { profile, label } = await preview(app, `target-selection-${boundary}`);
        await app.browser.execute((boundary) => {
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
          const matches = (value) =>
            boundary === "project_check"
              ? value?.appId === "alpha" && typeof value?.bytes === "number"
              : boundary === "projects_list"
                ? Array.isArray(value) && value.some((item) => item.id === "alpha" && item.appId === "alpha")
                : typeof value === "string" && value.startsWith("simulator-");
          callbacks.set = (id, callback) =>
            set.call(callbacks, id, (value) => {
              if (armed && matches(value)) {
                armed = false;
                probe.held = true;
                probe.release = () => callback(value);
              } else callback(value);
            });
        }, boundary);
        const { response } = queueRun(app, profile.id);
        await app.send("Open this simulator while I choose a different run target");
        await waitFor(
          () => app.browser.execute(() => window.__targetSelectionRace.held),
          `${boundary} response is held`,
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
        const selectedId = `${boundary}-selected`,
          hardwareId = `${boundary}-hardware`;
        const check = app.model.enqueue({
          tools: [
            { id: selectedId, name: "list_simulators", arguments: {} },
            { id: hardwareId, name: "push_to_device", arguments: {} },
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
        assert.equal(
          JSON.parse(results[selectedId]).selected,
          null,
          `${boundary}: the newer hardware selection stays current`,
        );
        assert.match(
          results[hardwareId],
          /disabled in native integration tests/,
          "legacy runs reach the guarded hardware branch",
        );
        await app.browser.switchToWindow(label);
        await app.browser.execute(() => window.__simulatorRace.release?.());
        await app.browser.switchToWindow("main");
        assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
      }
    },
    {
      devices: [
        { id: "physical-fixture", name: "Synthetic physical device", baseUrl: "https://device.example.invalid" },
      ],
    },
  ),
);
