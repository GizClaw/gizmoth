import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
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
  let reloaded = false;
  await waitFor(
    async () => {
      const result = await app.browser.execute((holdInspect) => {
        const host = document.querySelector(".game-preview");
        let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
        let player;
        for (; fiber && !player; fiber = fiber.return) {
          for (let hook = fiber.memoizedState; hook; hook = hook.next) {
            if (hook.memoizedState?.current?.executeCommand) player = hook.memoizedState.current;
          }
        }
        if (!player) return { ready: false };
        // Observe readiness without leaving a WebDriver callback pending when the
        // SDK rejects. Re-read the live controller on every poll after a Reload.
        if (window.__simulatorRaceReadiness?.player !== player) {
          const readiness = (window.__simulatorRaceReadiness = { player, ready: false });
          player.ready.then(
            () => {
              readiness.ready = true;
            },
            (error) => {
              readiness.reason = String(error);
            },
          );
        }
        const readiness = window.__simulatorRaceReadiness;
        if (readiness.retrying) return { ready: false };
        if (readiness.reason) return { ready: false, reason: readiness.reason };
        if (!readiness.ready) return { ready: false };
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
          controlRequests: [],
          appReplies: [],
          buttons: [],
          enableCapture() {
            const canvas = document.createElement("canvas");
            canvas.width = canvas.height = 240;
            const context = canvas.getContext("2d");
            context.fillStyle = "#126c4a";
            context.fillRect(0, 0, 240, 240);
            // A real canvas at the existing SDK seam exercises post-cancel capture transport.
            Object.defineProperty(player, "display", {
              configurable: true,
              get: () => ({ canvas, displayBrightnessPercent: 100 }),
            });
          },
        });
        player.viewer.buttons.subscribe((event) => probe.buttons.push({ ...event, time: Date.now() }));
        const callbacks = window.__TAURI_INTERNALS__.callbacks;
        const set = callbacks.set;
        const wrap = (callback) => (value) => {
          const payload = value?.payload;
          if (payload?.type === "gizmoth-simulator-retarget") {
            if (probe.delayCancellation && typeof payload.canceledRequestId === "string") {
              probe.cancellationHeld = true;
              probe.releaseCancellation = () => {
                probe.delayCancellation = false;
                callback(value);
              };
              return;
            }
            callback(value);
            if (probe.settleOnCancellation) {
              probe.cancelNotified = true;
              probe.canceledRequestId = payload.canceledRequestId;
              probe.settleOnCancellation = false;
              if (probe.releasePreparation) probe.releasePreparation();
              else probe.release?.();
            }
            return;
          }
          if (payload?.type === "gizmoth-simulator-reply") {
            probe.appReplies.push(payload.app?.id ?? null);
            if (probe.holdCanceledReplies && payload.app?.canceled) {
              probe.canceledReplyHeld = true;
              probe.releaseCanceledReply = () => callback(value);
              return;
            }
          }
          if (payload?.type === "gizmoth-simulator-control")
            probe.controlRequests.push({ requestId: payload.command.requestId, action: payload.command.action });
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
          if (
            probe.acceptCommands &&
            command.method === "tools.invoke" &&
            ["lua.app.install", "lua.app.run"].includes(command.tool)
          )
            return {};
          throw new Error("Unexpected command after cancellation");
        };
        return { ready: true };
      }, holdInspect);
      if (result.reason) {
        // CI's WebKit can lose a WebGL context during renderer creation. Retry only
        // that setup failure, before any assistant/UI race or instrumentation starts.
        if (reloaded || !/getShaderPrecisionFormat|Error creating WebGL context/.test(result.reason))
          throw new Error(`Simulator preview failed: ${result.reason}`);
        reloaded = true;
        console.warn(`Reloading ${profile.name} after WebGL initialization failed: ${result.reason}`);
        await app.browser.execute(() => {
          window.__simulatorRaceReadiness.retrying = true;
        });
        await app.browser.$(".preview-firmware-toggle").click();
        const reload = await app.browser.$('button[aria-label="重新加载预览"]');
        await reload.waitForEnabled();
        await reload.click();
        await app.browser.$(".preview-firmware-toggle").click();
      }
      return result.ready;
    },
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

for (const failure of ["declined", "validation"]) {
  test(`hardware target can be selected without dispatch after ${failure} simulator preparation`, options, (t) =>
    withApp(
      t,
      async (app) => {
        const profile = {
          id: randomUUID(),
          name: "Recovery selection",
          model: "h106-tiga",
          firmwareId: "",
          view: "screen",
        };
        assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
        await app.openProject("alpha");
        await app.idle();
        const original = await app.invoke("project_read", { id: "alpha", path: "app.json" });
        assert.equal(original.ok, true);
        if (failure === "validation") {
          await app.browser.$('button[aria-label="项目信息"]').click();
          await chooseSelect(app, "设备与模拟器运行", "allow");
          await app.browser.$('button[aria-label="关闭项目信息"]').click();
          assert.equal((await app.invoke("project_write", { id: "alpha", path: "app.json", content: "{" })).ok, true);
        }
        const { response } = queueRun(app, profile.id);
        await app.send("Prepare a local run that will not complete");
        if (failure === "declined") {
          const decline = await app.browser.$('//dialog//button[normalize-space()="拒绝"]');
          await decline.waitForDisplayed();
          await decline.click();
        }
        await finish(app, response);
        const failed = response.request.body.messages.filter((message) => message.role === "tool").at(-1);
        assert.match(
          failed.content,
          failure === "declined" ? /declined simulator testing/ : /app\.json|JSON|json|配置/,
        );
        app.model.enqueue({ tools: [{ id: "recovery-pending", name: "list_simulators", arguments: {} }] });
        const pending = app.model.enqueue({ text: "Incomplete local intent observed." });
        await app.send("Inspect the incomplete local selection");
        await finish(app, pending);
        const pendingResult = pending.request.body.messages.find(
          (message) => message.role === "tool" && message.tool_call_id === "recovery-pending",
        );
        assert.equal(JSON.parse(pendingResult.content).selectionPending, true);
        if (failure === "validation")
          assert.equal(
            (await app.invoke("project_write", { id: "alpha", path: "app.json", content: original.value })).ok,
            true,
          );
        await app.browser.$('button[aria-label="项目信息"]').click();
        await chooseSelect(app, "设备与模拟器运行", "ask");
        await app.browser.$('button[aria-label="关闭项目信息"]').click();
        await app.browser.execute(() => {
          const callbacks = window.__TAURI_INTERNALS__.callbacks;
          const set = callbacks.set;
          const probe = (window.__selectionOnlyIo = {
            dispatched: [],
            restore: () => {
              callbacks.set = set;
            },
          });
          callbacks.set = (id, callback) =>
            set.call(callbacks, id, (value) => {
              const match = String(value).match(
                /This operation is disabled in native integration tests: (project_install|run_app)$/,
              );
              if (match) probe.dispatched.push(match[1]);
              callback(value);
            });
        });
        await app.browser.$('button[aria-label="运行"]').click();
        const select = await app.browser.$('button.run-target-select-only[data-device-id="recovery-device"]');
        await select.waitForDisplayed();
        await select.click();
        await waitFor(
          async () => (await app.browser.$("body").getText()).includes("未安装或运行"),
          "selection-only feedback appears",
        );
        assert.deepEqual(
          await app.browser.execute(() => window.__selectionOnlyIo.dispatched),
          [],
          "selecting hardware sends no install/run IPC",
        );
        app.model.enqueue({
          tools: [
            { id: "recovery-ready", name: "list_simulators", arguments: {} },
            { id: "recovery-hardware", name: "push_to_device", arguments: {} },
          ],
        });
        const recovered = app.model.enqueue({ text: "Hardware recovery remains separately confirmed." });
        await app.send("Resume the assistant's hardware flow after selection only");
        const declineInstall = await app.browser.$('//dialog//button[normalize-space()="拒绝"]');
        await declineInstall.waitForDisplayed();
        await declineInstall.click();
        await finish(app, recovered);
        const results = Object.fromEntries(
          recovered.request.body.messages
            .filter((message) => message.role === "tool")
            .map((message) => [message.tool_call_id, message.content]),
        );
        assert.equal(JSON.parse(results["recovery-ready"]).selectionPending, false);
        assert.equal(JSON.parse(results["recovery-ready"]).selected, null);
        assert.match(
          results["recovery-hardware"],
          /The user declined the install/,
          "hardware execution still has its own consent",
        );
        assert.deepEqual(await app.browser.execute(() => window.__selectionOnlyIo.dispatched), []);
        await app.browser.execute(() => window.__selectionOnlyIo.restore());
        assert.deepEqual(await app.browser.getWindowHandles(), ["main"]);
        assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
      },
      {
        devices: [
          { id: "recovery-device", name: "Synthetic recovery device", baseUrl: "https://device.example.invalid" },
        ],
      },
    ),
  );
}

for (const priorTarget of [false, true]) {
  test(
    `pending simulator selection blocks legacy fallback with ${priorTarget ? "an older simulator" : "no committed target"}`,
    options,
    (t) =>
      withApp(
        t,
        async (app) => {
          let previous;
          if (priorTarget) {
            previous = await preview(app, "previous-local-target");
            await userRun(app, previous.profile);
            await app.browser.switchToWindow(previous.label);
            await waitFor(
              () => app.browser.execute(() => window.__simulatorRace.entered),
              "the earlier simulator run is queued",
            );
            await app.browser.switchToWindow("main");
          }
          const next = await preview(app, "pending-local-target");
          await holdRunBoundary(app);
          await userRun(app, next.profile);
          await waitFor(
            () => app.browser.execute(() => window.__toolOpeningRace.held),
            "the new simulator open is still pending",
          );
          app.model.enqueue({
            tools: [
              { id: "pending-push", name: "push_to_device", arguments: {} },
              { id: "pending-list", name: "list_simulators", arguments: {} },
              { id: "pending-default-read", name: "inspect_simulator", arguments: {} },
              ...(previous
                ? [
                    {
                      id: "pending-explicit-read",
                      name: "inspect_simulator",
                      arguments: { profileId: previous.profile.id },
                    },
                  ]
                : []),
            ],
          });
          const checked = app.model.enqueue({ text: "Pending simulator intent checked." });
          await app.send("Try the legacy run tool while my simulator window is still opening");
          await finish(app, checked);
          const results = Object.fromEntries(
            checked.request.body.messages
              .filter((message) => message.role === "tool")
              .map((message) => [message.tool_call_id, message.content]),
          );
          assert.match(
            results["pending-push"],
            /Simulator target selection is pending or incomplete/,
            "pending local intent never routes to hardware or the older simulator",
          );
          assert.match(results["pending-default-read"], /Simulator target selection is pending or incomplete/);
          const selection = JSON.parse(results["pending-list"]);
          assert.equal(selection.selectionPending, true);
          assert.equal(selection.selected, previous?.profile.id ?? null);
          if (previous) assert.equal(JSON.parse(results["pending-explicit-read"]).profileId, previous.profile.id);
          await app.browser.execute(() => window.__toolOpeningRace.release());
          await app.browser.switchToWindow(next.label);
          await waitFor(
            () => app.browser.execute(() => window.__simulatorRace.entered),
            "the chosen simulator starts after opening completes",
          );
          await app.browser.switchToWindow("main");
          app.model.enqueue({
            tools: [
              { id: "ready-list", name: "list_simulators", arguments: {} },
              { id: "ready-read", name: "inspect_simulator", arguments: {} },
            ],
          });
          const ready = app.model.enqueue({ text: "Completed local selection checked." });
          await app.send("Inspect the completed simulator selection without an explicit ID");
          await finish(app, ready);
          const readyResults = Object.fromEntries(
            ready.request.body.messages
              .filter((message) => message.role === "tool")
              .map((message) => [message.tool_call_id, message.content]),
          );
          assert.equal(JSON.parse(readyResults["ready-list"]).selectionPending, false);
          assert.equal(JSON.parse(readyResults["ready-list"]).selected, next.profile.id);
          assert.equal(JSON.parse(readyResults["ready-read"]).profileId, next.profile.id);
          for (const target of [previous, next].filter(Boolean)) {
            await app.browser.switchToWindow(target.label);
            await app.browser.$("button=停止等待").click();
            await app.browser.$(".simulator-delete-text").waitForEnabled();
            await app.browser.execute(() => window.__simulatorRace.release?.());
            await app.browser.switchToWindow("main");
            assert.equal((await app.invoke("simulator_profile_delete", { id: target.profile.id })).ok, true);
          }
        },
        {
          devices: [
            { id: "pending-hardware", name: "Synthetic physical fallback", baseUrl: "https://device.example.invalid" },
          ],
        },
      ),
  );
}

for (const boundary of ["validation", "installation"]) {
  test(`hardware dispatch rejects a newer simulator choice during ${boundary}`, options, (t) =>
    withApp(
      t,
      async (app) => {
        const { profile, label } = await preview(app, `hardware-${boundary}`);
        if (boundary === "validation") await holdRunBoundary(app, "project_check");
        else
          await app.browser.execute(() => {
            const callbacks = window.__TAURI_INTERNALS__.callbacks;
            const set = callbacks.set;
            let previous;
            const probe = (window.__hardwareInstallRace = { held: false, release: null, runs: 0 });
            callbacks.set = (id, callback) => {
              // invoke registers its resolve and reject callbacks synchronously in order.
              const resolve = previous;
              previous = callback;
              return set.call(callbacks, id, (value) => {
                const message = String(value);
                if (message === "This operation is disabled in native integration tests: project_install") {
                  probe.held = true;
                  // Hold a synthetic accepted-install acknowledgement only after the
                  // native guard rejects the actual install command.
                  probe.release = () => {
                    callbacks.delete(id);
                    resolve(undefined);
                  };
                  return;
                }
                if (message === "This operation is disabled in native integration tests: run_app") probe.runs++;
                callback(value);
              });
            };
            probe.restore = () => {
              callbacks.set = set;
            };
          });
        const id = `hardware-target-${boundary}`;
        app.model.enqueue({ tools: [{ id, name: "push_to_device", arguments: {} }] });
        const response = app.model.enqueue({ text: "Hardware target change checked." });
        await app.send("Prepare a hardware dispatch while I select a simulator");
        await waitFor(
          () =>
            app.browser.execute(
              (boundary) =>
                boundary === "validation" ? window.__toolOpeningRace.held : window.__hardwareInstallRace.held,
              boundary,
            ),
          `${boundary} is pending`,
        );
        await userRun(app, profile);
        await app.browser.switchToWindow(label);
        await waitFor(
          () => app.browser.execute(() => window.__simulatorRace.entered),
          "the newer simulator UI run is queued",
        );
        await app.browser.switchToWindow("main");
        await app.browser.execute(
          (boundary) =>
            boundary === "validation" ? window.__toolOpeningRace.release() : window.__hardwareInstallRace.release(),
          boundary,
        );
        await finish(app, response);
        const result = response.request.body.messages.find(
          (message) => message.role === "tool" && message.tool_call_id === id,
        );
        assert.match(
          result.content,
          /Run target changed/,
          "the older invocation never dispatches to its captured physical target",
        );
        if (boundary === "installation") {
          assert.equal(
            await app.browser.execute(() => window.__hardwareInstallRace.runs),
            0,
            "no physical run IPC follows the superseded install acknowledgement",
          );
          await app.browser.execute(() => window.__hardwareInstallRace.restore());
        }
        await app.browser.switchToWindow(label);
        await app.browser.execute(() => window.__simulatorRace.release());
        await app.browser.switchToWindow("main");
        assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
      },
      {
        devices: [
          { id: "target-fixture", name: "Synthetic hardware target", baseUrl: "https://device.example.invalid" },
        ],
      },
    ),
  );
}

test("overlapping UI and assistant requests share the real native opening reservation", options, (t) =>
  withApp(
    t,
    async (app) => {
      const gate = join(app.data, "release-simulator-opening");
      const profile = {
        id: randomUUID(),
        name: "Shared native opening",
        model: "h106-tiga",
        firmwareId: "",
        view: "screen",
      };
      const label = `simulator-${profile.id.replaceAll("-", "")}`;
      assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
      await app.openProject("alpha");
      await app.idle();
      await app.browser.$('button[aria-label="项目信息"]').click();
      await chooseSelect(app, "设备与模拟器运行", "allow");
      await app.browser.$('button[aria-label="关闭项目信息"]').click();
      const { response } = queueRun(app, profile.id);
      try {
        await app.send("Start a simulator whose native opening reservation stays held");
        await waitFor(
          async () => (await app.browser.getWindowHandles()).includes(label),
          "the registered window still owns OPENING",
        );
        await app.browser.switchToWindow(label);
        await app.browser.$(".game-preview").waitForDisplayed();
        await waitFor(
          () =>
            runNativeAsync(app.browser, async (done) => {
              const host = document.querySelector(".game-preview");
              let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
              let player;
              for (; fiber && !player; fiber = fiber.return)
                for (let hook = fiber.memoizedState; hook; hook = hook.next)
                  if (hook.memoizedState?.current?.executeCommand) player = hook.memoizedState.current;
              if (!player) return done(false);
              await player.ready;
              const initial = player.state;
              const probe = (window.__sharedOpening = { entered: false, starts: 0, phase: "idle", release: null });
              Object.defineProperty(player, "state", {
                configurable: true,
                get: () => ({ ...initial, mode: probe.entered ? "play" : "preview", phase: probe.phase }),
              });
              player.enter = async () => {
                probe.entered = true;
                probe.phase = "installing";
                await new Promise((resolve) => {
                  probe.release = resolve;
                });
                probe.phase = "ready";
              };
              player.start = () => {
                probe.starts++;
                probe.phase = "running";
              };
              player.executeCommand = async () => {
                throw new Error("Fixture command seam");
              };
              done(true);
            }),
          "the new window is instrumented while native opening remains reserved",
          45000,
        );
        await userRun(app, profile);
        await writeFile(gate, "release");
        await finish(app, response);
        const oldResult = response.request.body.messages.filter((message) => message.role === "tool").at(-1);
        assert.match(oldResult.content, /canceled or superseded/);
        await app.browser.switchToWindow(label);
        await waitFor(
          () => app.browser.execute(() => window.__sharedOpening.entered),
          "the successor receives its Run snapshot after native completion",
        );
        await app.browser.switchToWindow("main");
        app.model.enqueue({
          tools: [
            { id: "shared-route", name: "list_simulators", arguments: {} },
            { id: "shared-inspection", name: "inspect_simulator", arguments: {} },
          ],
        });
        const checked = app.model.enqueue({ text: "Shared opening successor checked." });
        await app.send("Check the successor's committed target and inspection binding");
        await finish(app, checked);
        const results = checked.request.body.messages.filter((message) => message.role === "tool");
        assert.equal(
          JSON.parse(results.find((message) => message.tool_call_id === "shared-route").content).selected,
          profile.id,
        );
        assert.equal(
          JSON.parse(results.find((message) => message.tool_call_id === "shared-inspection").content).profileId,
          profile.id,
        );
        await app.browser.switchToWindow(label);
        await app.browser.execute(() => window.__sharedOpening.release());
        await waitFor(
          () => app.browser.execute(() => window.__sharedOpening.starts === 1),
          "the newer UI request may power on",
        );
        await app.browser.switchToWindow("main");
        assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
      } finally {
        await writeFile(gate, "release").catch(() => {});
      }
    },
    { holdSimulatorOpening: "manual" },
  ),
);

test("changing simulator selection during native opening rejects the older run", options, (t) =>
  withApp(t, async (app) => {
    const first = await preview(app, "generation-first");
    const second = await preview(app, "generation-second");
    await holdRunBoundary(app);
    const { response } = queueRun(app, first.profile.id);
    await app.send("Open the first simulator while I select the second one");
    await waitFor(() => app.browser.execute(() => window.__toolOpeningRace.held), "the first native open is pending");
    await userRun(app, second.profile);
    await app.browser.switchToWindow(second.label);
    await waitFor(() => app.browser.execute(() => window.__simulatorRace.entered), "the second UI run is queued");
    await app.browser.switchToWindow("main");
    await app.browser.execute(() => window.__toolOpeningRace.release());
    await finish(app, response);
    const oldResult = response.request.body.messages.filter((message) => message.role === "tool").at(-1);
    assert.match(oldResult.content, /canceled or superseded/, "an outdated generation never reports a successful run");
    app.model.enqueue({ tools: [{ id: "implicit-new-target", name: "inspect_simulator", arguments: {} }] });
    const checked = app.model.enqueue({ text: "Current target checked." });
    await app.send("Inspect the implicitly selected simulator");
    await finish(app, checked);
    const result = checked.request.body.messages.find(
      (message) => message.role === "tool" && message.tool_call_id === "implicit-new-target",
    );
    assert.equal(JSON.parse(result.content).profileId, second.profile.id);
    await app.browser.switchToWindow(first.label);
    await app.browser.execute(() => window.__simulatorRace.release?.());
    const oldCalls = await runNativeAsync(app.browser, (done) =>
      setTimeout(() => done({ starts: window.__simulatorRace.starts, commands: window.__simulatorRace.commands }), 100),
    );
    assert.deepEqual(oldCalls, { starts: 0, commands: 0 });
    await app.browser.switchToWindow(second.label);
    await app.browser.execute(() => window.__simulatorRace.release());
    await waitFor(
      () => app.browser.execute(() => window.__simulatorRace.starts === 1),
      "the newer UI run still powers on",
    );
    await app.browser.switchToWindow("main");
    for (const profile of [first.profile, second.profile])
      assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("repeated denied permission updates preserve in-flight read-only inspection", options, (t) =>
  withApp(t, async (app) => {
    const { profile, label } = await preview(app, "repeated-denial");
    const { response } = queueRun(app, profile.id);
    await app.send("Queue a simulator before denying execution");
    await finish(app, response);
    await app.browser.$('button[aria-label="项目信息"]').click();
    await chooseSelect(app, "设备与模拟器运行", "deny");
    await app.browser.$('button[aria-label="关闭项目信息"]').click();
    await app.browser.switchToWindow(label);
    await waitFor(
      async () => !(await app.browser.$(".preview-incoming-app").isExisting()),
      "the first denial cancels execution",
    );
    const previous = await app.browser.execute(() => window.__simulatorRace.controlRequests.length);
    await app.browser.switchToWindow("main");
    app.model.enqueue({
      tools: [
        { id: "repeated-denial-read", name: "inspect_simulator", arguments: { profileId: profile.id, waitMs: 3000 } },
      ],
    });
    const checked = app.model.enqueue({ text: "Repeated permission update checked." });
    await app.send("Inspect the canceled run while I update another permission");
    await app.browser.switchToWindow(label);
    await waitFor(
      () =>
        app.browser.execute(
          (previous) =>
            window.__simulatorRace.controlRequests.slice(previous).some((control) => control.action === "inspect"),
          previous,
        ),
      "the read-only inspection is active",
    );
    await app.browser.switchToWindow("main");
    await app.browser.$('button[aria-label="项目信息"]').click();
    await chooseSelect(app, "联网", "deny");
    await app.browser.$('button[aria-label="关闭项目信息"]').click();
    await finish(app, checked);
    const result = checked.request.body.messages.find(
      (message) => message.role === "tool" && message.tool_call_id === "repeated-denial-read",
    );
    assert.equal(
      JSON.parse(result.content).app.status,
      "canceled",
      "an already-invalidated execution sends no unscoped cancellation",
    );
    await app.browser.switchToWindow(label);
    await app.browser.execute(() => window.__simulatorRace.release?.());
    await app.browser.switchToWindow("main");
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("text-only models receive capture metadata without image data", options, (t) =>
  withApp(t, async (app) => {
    const { profile, label } = await preview(app, "text-metadata");
    await app.browser.$('aside.agent button[aria-label^="选择模型："]').click();
    const plain = await app.browser.$('div[role="option"][title^="E2E Plain"]');
    await plain.waitForDisplayed();
    await plain.click();
    await app.browser.$('aside.agent button[aria-label^="选择模型："]').waitForEnabled();
    await app.browser.switchToWindow(label);
    await app.browser.execute(() => window.__simulatorRace.enableCapture());
    await app.browser.switchToWindow("main");
    const { response } = queueRun(app, profile.id);
    await app.send("Start a simulator for metadata-only inspection");
    await finish(app, response);
    app.model.enqueue({
      tools: [{ id: "text-capture", name: "inspect_simulator", arguments: { profileId: profile.id, capture: true } }],
    });
    const checked = app.model.enqueue({ text: "Screen metadata checked without vision." });
    await app.send("Capture dimensions, brightness and hash without forwarding pixels");
    await finish(app, checked);
    assert.equal(checked.request.body.model, "plain-fixture", "the regression uses a genuinely text-only model");
    const result = checked.request.body.messages.find(
      (message) => message.role === "tool" && message.tool_call_id === "text-capture",
    );
    const status = JSON.parse(result.content);
    assert.equal(status.screen.width, 240);
    assert.equal(status.screen.height, 240);
    assert.equal(status.screen.brightness, 100);
    assert.match(status.screen.hash, /^[a-f0-9]{8}$/);
    assert.equal(status.screen.data, undefined);
    assert.doesNotMatch(
      JSON.stringify(checked.request.body.messages),
      /data:image\/png;base64,/,
      "no PNG reaches the text-only model",
    );
    await app.browser.switchToWindow(label);
    await app.browser.$("button=停止等待").click();
    await app.browser.$(".simulator-delete-text").waitForEnabled();
    await app.browser.execute(() => window.__simulatorRace.release?.());
    await app.browser.switchToWindow("main");
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("a startup control failure cancels its independently queued app request", options, (t) =>
  withApp(t, async (app) => {
    const { profile, label } = await preview(app, "failed-start-control");
    await app.browser.switchToWindow(label);
    const wrapped = await app.browser.execute(() => {
      const host = document.querySelector(".game-preview");
      let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
      for (; fiber; fiber = fiber.return) {
        for (let hook = fiber.memoizedState; hook; hook = hook.next) {
          const ref = hook.memoizedState;
          if (typeof ref?.current === "function" && ref.current.length === 2) {
            const original = ref.current;
            ref.current = (command, signal) => {
              if (command.action === "start") throw new Error("Simulator is busy changing firmware.");
              return original(command, signal);
            };
            return true;
          }
        }
      }
      return false;
    });
    assert.equal(wrapped, true, "the actual simulator control receiver is wrapped");
    await app.browser.switchToWindow("main");
    const { response } = queueRun(app, profile.id);
    await app.send("Start this simulator while its start control reports an error");
    await finish(app, response);
    const failed = response.request.body.messages.filter((message) => message.role === "tool").at(-1);
    assert.match(failed.content, /busy changing firmware/);
    app.model.enqueue({
      tools: [{ id: "failed-start-status", name: "inspect_simulator", arguments: { profileId: profile.id } }],
    });
    const checked = app.model.enqueue({ text: "Failed startup request checked." });
    await app.send("Inspect the app request after the startup tool failed");
    await finish(app, checked);
    const result = checked.request.body.messages.find(
      (message) => message.role === "tool" && message.tool_call_id === "failed-start-status",
    );
    assert.equal(
      JSON.parse(result.content).app.status,
      "canceled",
      "a failed tool does not leave an active queued app",
    );
    await app.browser.switchToWindow(label);
    await app.browser.execute(() => window.__simulatorRace.release?.());
    const observed = await runNativeAsync(app.browser, (done) =>
      setTimeout(() => done({ starts: window.__simulatorRace.starts, commands: window.__simulatorRace.commands }), 100),
    );
    assert.deepEqual(observed, { starts: 0, commands: 0 });
    await app.browser.switchToWindow("main");
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("a stale native opening cannot interrupt a newer project's inspection", options, (t) =>
  withApp(t, async (app) => {
    const { profile, label } = await preview(app, "stale-open-control");
    await holdRunBoundary(app);
    queueRun(app, profile.id, false);
    await app.send("Open Alpha's simulator while the native result is delayed");
    await waitFor(
      () => app.browser.execute(() => window.__toolOpeningRace.held),
      "Alpha's native open response is held",
    );
    await app.openProject("beta");
    await app.idle();
    await userRun(app, profile);
    const alphaDone = app.model.enqueue({ text: "Alpha's stale open was rejected." });
    await app.browser.execute(() => window.__toolOpeningRace.release());
    await waitFor(() => !!alphaDone.request, "Alpha's superseded run settles");
    const alphaResult = alphaDone.request.body.messages.filter((message) => message.role === "tool").at(-1);
    assert.match(alphaResult.content, /canceled or superseded/);
    await app.browser.switchToWindow(label);
    await waitFor(
      () => app.browser.execute(() => window.__simulatorRace.entered),
      "Beta's UI run is queued after the shared opening completes",
    );
    const successor = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
    await app.browser.switchToWindow("main");
    app.model.enqueue({
      tools: [{ id: "beta-inspection", name: "inspect_simulator", arguments: { profileId: profile.id, waitMs: 3000 } }],
    });
    const betaDone = app.model.enqueue({ text: "Beta's inspection was preserved." });
    await app.send("Inspect Beta's UI run after the shared native opening completes");
    await finish(app, betaDone);
    const betaResult = betaDone.request.body.messages.find(
      (message) => message.role === "tool" && message.tool_call_id === "beta-inspection",
    );
    assert.equal(
      JSON.parse(betaResult.content).requestId,
      successor,
      "stale completion never broadcasts an unscoped retarget",
    );
    await app.browser.switchToWindow(label);
    await app.browser.execute(() => window.__simulatorRace.release());
    await waitFor(
      () => app.browser.execute(() => window.__simulatorRace.starts === 1),
      "Beta's newer UI run still powers on",
    );
    await app.browser.switchToWindow("main");
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("execution revocation preserves an already active read-only inspection", options, (t) =>
  withApp(t, async (app) => {
    const { profile, label } = await preview(app, "inflight-inspection");
    const { response } = queueRun(app, profile.id);
    await app.send("Start a simulator before changing execution permission");
    await finish(app, response);
    await app.browser.switchToWindow(label);
    await waitFor(() => app.browser.execute(() => window.__simulatorRace.entered), "startup is pending");
    const requestId = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
    const previous = await app.browser.execute(() => window.__simulatorRace.controlRequests.length);
    await app.browser.switchToWindow("main");
    app.model.enqueue({
      tools: [{ id: "inflight-read", name: "inspect_simulator", arguments: { profileId: profile.id, waitMs: 3000 } }],
    });
    const checked = app.model.enqueue({ text: "Read-only inspection survived execution revocation." });
    await app.send("Keep inspecting while I revoke execution access");
    await app.browser.switchToWindow(label);
    await waitFor(
      () =>
        app.browser.execute(
          (previous) =>
            window.__simulatorRace.controlRequests.slice(previous).some((control) => control.action === "inspect"),
          previous,
        ),
      "the inspection is in flight",
    );
    await app.browser.switchToWindow("main");
    await app.browser.$('button[aria-label="项目信息"]').click();
    await chooseSelect(app, "设备与模拟器运行", "deny");
    await app.browser.$('button[aria-label="关闭项目信息"]').click();
    await finish(app, checked);
    const result = checked.request.body.messages.find(
      (message) => message.role === "tool" && message.tool_call_id === "inflight-read",
    );
    const status = JSON.parse(result.content);
    assert.equal(status.requestId, requestId);
    assert.equal(status.app.status, "canceled", "the same read finishes with the cancellation outcome");
    await app.browser.switchToWindow(label);
    await app.browser.execute(() => window.__simulatorRace.release());
    const observed = await runNativeAsync(app.browser, (done) =>
      setTimeout(() => done({ starts: window.__simulatorRace.starts, commands: window.__simulatorRace.commands }), 100),
    );
    assert.deepEqual(observed, { starts: 0, commands: 0 });
    await app.browser.switchToWindow("main");
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("app cancellation aborts waiters before delayed snapshot replies", options, (t) =>
  withApp(t, async (app) => {
    for (const boundary of ["startup", "package"]) {
      for (const reason of ["stop", "deny"]) {
        const { profile, label } = await preview(app, `direct-${boundary}-${reason}`);
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
        queueRun(app, profile.id, false);
        const response = app.model.enqueue(
          reason === "stop" ? { hold: true, thinking: "Waiting for cancellation." } : { text: "Run queued." },
        );
        await app.send("Queue a run whose cancellation snapshot will be delayed");
        await waitFor(() => !!response.request, "the assistant run status is returned");
        if (reason === "deny") await app.idle();
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
        await app.browser.execute((boundary) => {
          const probe = window.__simulatorRace;
          probe.holdCanceledReplies = true;
          probe.settleOnCancellation = true;
          if (boundary === "package") probe.releasePreparation = () => window.__toolOpeningRace.release();
        }, boundary);
        await app.browser.switchToWindow("main");
        if (reason === "stop") {
          await stop(app);
          await app.idle();
          response.release();
        } else {
          await app.browser.$('button[aria-label="项目信息"]').click();
          await chooseSelect(app, "设备与模拟器运行", "deny");
          await app.browser.$('button[aria-label="关闭项目信息"]').click();
        }
        await app.browser.switchToWindow(label);
        await waitFor(
          () =>
            app.browser.execute(
              () => window.__simulatorRace.cancelNotified && window.__simulatorRace.canceledReplyHeld,
            ),
          "notification arrives while the canceled snapshot stays blocked",
        );
        const observed = await runNativeAsync(app.browser, (done) =>
          setTimeout(
            () =>
              done({
                starts: window.__simulatorRace.starts,
                commands: window.__simulatorRace.commands,
                requestId: window.__simulatorRace.canceledRequestId,
              }),
            100,
          ),
        );
        assert.deepEqual(
          observed,
          { starts: 0, commands: 0, requestId },
          "delivering cancellation prevents power-on and install/run before snapshot reload",
        );
        await app.browser.switchToWindow("main");
        const id = `direct-status-${boundary}-${reason}`;
        app.model.enqueue({ tools: [{ id, name: "inspect_simulator", arguments: { profileId: profile.id } }] });
        const checked = app.model.enqueue({ text: "Immediate cancellation state checked." });
        await app.send("Read cancellation status before the delayed snapshot is applied");
        await finish(app, checked);
        const result = checked.request.body.messages.find(
          (message) => message.role === "tool" && message.tool_call_id === id,
        );
        const status = JSON.parse(result.content);
        assert.equal(status.requestId, requestId);
        assert.equal(status.app.status, "canceled");
        await app.browser.switchToWindow(label);
        await app.browser.execute(() => window.__simulatorRace.releaseCanceledReply());
        await app.browser.switchToWindow("main");
        assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
      }
    }
  }),
);

test("late app cancellation preserves newer UI waiters and controls", options, (t) =>
  withApp(t, async (app) => {
    const { profile, label } = await preview(app, "late-direct-cancel");
    const { response } = queueRun(app, profile.id);
    await app.send("Queue an assistant run before I replace it with UI Run");
    await finish(app, response);
    await app.browser.switchToWindow(label);
    await waitFor(() => app.browser.execute(() => window.__simulatorRace.entered), "the assistant startup is pending");
    const original = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
    await app.browser.execute(() => {
      window.__simulatorRace.delayCancellation = true;
    });
    await app.browser.switchToWindow("main");
    await app.browser.$('button[aria-label="项目信息"]').click();
    await chooseSelect(app, "设备与模拟器运行", "deny");
    await chooseSelect(app, "设备与模拟器运行", "allow");
    await app.browser.$('button[aria-label="关闭项目信息"]').click();
    await app.browser.switchToWindow(label);
    await waitFor(
      () => app.browser.execute(() => window.__simulatorRace.cancellationHeld),
      "the old cancellation notification is held",
    );
    await userRun(app, profile);
    await app.browser.switchToWindow(label);
    await waitFor(
      () =>
        app.browser.execute(
          (original) => window.__simulatorRace.appReplies.some((id) => id && id !== original),
          original,
        ),
      "the newer UI request is bound",
    );
    const successor = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
    await app.browser.switchToWindow("main");
    app.model.enqueue({
      tools: [
        { id: "successor-inspection", name: "inspect_simulator", arguments: { profileId: profile.id, waitMs: 3000 } },
      ],
    });
    const checked = app.model.enqueue({ text: "Successor control preserved." });
    await app.send("Inspect the new UI run while an old cancellation arrives");
    await app.browser.switchToWindow(label);
    await waitFor(
      () =>
        app.browser.execute(
          (successor) =>
            window.__simulatorRace.controlRequests.some(
              (control) => control.action === "inspect" && control.requestId === successor,
            ),
          successor,
        ),
      "the newer inspection control is active",
    );
    await app.browser.execute(() => window.__simulatorRace.releaseCancellation());
    await app.browser.switchToWindow("main");
    await finish(app, checked);
    const result = checked.request.body.messages.find(
      (message) => message.role === "tool" && message.tool_call_id === "successor-inspection",
    );
    const status = JSON.parse(result.content);
    assert.equal(status.requestId, successor, "the late notification does not abort the newer control");
    assert.notEqual(status.app.status, "canceled");
    await app.browser.switchToWindow(label);
    assert.equal(await app.browser.$(".preview-incoming-app").isExisting(), true, "the newer UI waiter remains valid");
    await app.browser.execute(() => window.__simulatorRace.release());
    await waitFor(
      () => app.browser.execute(() => window.__simulatorRace.starts === 1),
      "the newer UI request may power on",
    );
    await app.browser.switchToWindow("main");
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }),
);

test("restoring run permission never revives a revoked preparation", options, (t) =>
  withApp(
    t,
    async (app) => {
      await app.openProject("alpha");
      await app.idle();
      for (const [boundary, restored] of ["project_check", "projects_list"].flatMap((boundary) =>
        ["ask", "allow"].map((restored) => [boundary, restored]),
      )) {
        const profile = {
          id: randomUUID(),
          name: "Revocation preparation",
          model: "h106-tiga",
          firmwareId: "",
          view: "screen",
        };
        assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
        await app.browser.$('button[aria-label="项目信息"]').click();
        await chooseSelect(app, "设备与模拟器运行", "ask");
        await app.browser.$('button[aria-label="关闭项目信息"]').click();
        await holdRunBoundary(app, boundary);
        const { response } = queueRun(app, profile.id);
        await app.send("Confirm a simulator run while preparation is pending");
        const confirm = await app.browser.$('//dialog//button[normalize-space()="允许"]');
        await confirm.waitForDisplayed();
        await confirm.click();
        await waitFor(
          () => app.browser.execute(() => window.__toolOpeningRace.held),
          "confirmed preparation is pending",
        );
        await app.browser.$('button[aria-label="项目信息"]').click();
        await chooseSelect(app, "设备与模拟器运行", "deny");
        await chooseSelect(app, "设备与模拟器运行", restored);
        await app.browser.$('button[aria-label="关闭项目信息"]').click();
        await app.browser.execute(() => window.__toolOpeningRace.release());
        await finish(app, response);
        const result = response.request.body.messages.filter((message) => message.role === "tool").at(-1);
        assert.match(
          result.content,
          /Run permission was revoked/,
          `${boundary}: ${restored} never restores an old confirmation`,
        );
        assert.equal(await app.browser.$("dialog[open]").isExisting(), false);
        assert.deepEqual(await app.browser.getWindowHandles(), ["main"]);
        assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
      }
      await app.browser.$('button[aria-label="项目信息"]').click();
      await chooseSelect(app, "设备与模拟器运行", "allow");
      await app.browser.$('button[aria-label="关闭项目信息"]').click();
      // A canceled local preparation retains local intent until the user explicitly
      // chooses hardware; permission restoration alone must not change routing.
      await app.browser.$('button[aria-label="运行"]').click();
      await app.browser.$('button.run-target-select-only[data-device-id="revocation-fixture"]').click();
      await waitFor(
        async () => (await app.browser.$("body").getText()).includes("未安装或运行"),
        "the explicit hardware choice completes without dispatch",
      );
      await holdRunBoundary(app, "project_check");
      app.model.enqueue({ tools: [{ id: "hardware-revocation", name: "push_to_device", arguments: {} }] });
      const response = app.model.enqueue({ text: "Hardware revocation observed." });
      await app.send("Prepare a hardware run while I revoke and restore permission");
      await waitFor(() => app.browser.execute(() => window.__toolOpeningRace.held), "hardware validation is pending");
      await app.browser.$('button[aria-label="项目信息"]').click();
      await chooseSelect(app, "设备与模拟器运行", "deny");
      await chooseSelect(app, "设备与模拟器运行", "allow");
      await app.browser.$('button[aria-label="关闭项目信息"]').click();
      await app.browser.execute(() => window.__toolOpeningRace.release());
      await finish(app, response);
      const result = response.request.body.messages.find(
        (message) => message.role === "tool" && message.tool_call_id === "hardware-revocation",
      );
      assert.match(
        result.content,
        /Run permission was revoked/,
        "the hardware path also rejects revoked preparations before device IPC",
      );
    },
    { devices: [{ id: "revocation-fixture", name: "Revocation device", baseUrl: "https://device.example.invalid" }] },
  ),
);

test("canceled simulator runs retain project-scoped inspection and capture", options, (t) =>
  withApp(
    t,
    async (app) => {
      for (const reason of ["stop", "deny"]) {
        const { profile, label } = await preview(app, `readonly-${reason}`);
        await app.browser.switchToWindow(label);
        await app.browser.execute(() => window.__simulatorRace.enableCapture());
        await app.browser.switchToWindow("main");
        queueRun(app, profile.id, false);
        const response = app.model.enqueue(
          reason === "stop" ? { hold: true, thinking: "Startup is pending." } : { text: "Startup queued." },
        );
        await app.send("Start a simulator whose canceled state I will inspect");
        await waitFor(() => !!response.request, "the run tool returned its status");
        if (reason === "deny") await app.idle();
        await app.browser.switchToWindow(label);
        await waitFor(() => app.browser.execute(() => window.__simulatorRace.entered), "installation is pending");
        const requestId = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
        await app.browser.switchToWindow("main");
        if (reason === "stop") {
          await stop(app);
          await app.idle();
          response.release();
        } else {
          await app.browser.$('button[aria-label="项目信息"]').click();
          await chooseSelect(app, "设备与模拟器运行", "deny");
          await app.browser.$('button[aria-label="关闭项目信息"]').click();
        }
        await app.browser.switchToWindow(label);
        await waitFor(
          async () => !(await app.browser.$(".preview-incoming-app").isExisting()),
          "the execution request is canceled",
        );
        await app.browser.$(".simulator-delete-text").waitForEnabled();
        await app.browser.switchToWindow("main");
        const inspectId = `readonly-${reason}`,
          inputId = `blocked-${reason}`;
        app.model.enqueue({
          tools: [
            { id: inspectId, name: "inspect_simulator", arguments: { profileId: profile.id, capture: true } },
            { id: inputId, name: "press_simulator_button", arguments: { profileId: profile.id, button: "ok" } },
          ],
        });
        const checked = app.model.enqueue({ text: "Read-only cancellation state checked." });
        await app.send("Read status and capture after cancellation, then try disallowed input");
        await finish(app, checked);
        const messages = checked.request.body.messages;
        const inspected = messages.find((message) => message.role === "tool" && message.tool_call_id === inspectId);
        const status = JSON.parse(inspected.content);
        assert.equal(status.requestId, requestId);
        assert.equal(status.app.status, "canceled");
        assert.equal(status.screen.width, 240);
        assert.equal(status.screen.height, 240);
        // The SDK appends tool images after the consecutive tool results, in a user message.
        assert.match(
          JSON.stringify(messages.slice(messages.indexOf(inspected) + 1)),
          /data:image\/png;base64,/,
          "the model receives the current inspection's captured canvas after cancellation",
        );
        const input = messages.find((message) => message.role === "tool" && message.tool_call_id === inputId);
        assert.match(input.content, /disabled|Run this project/, "a read-only binding never authorizes input");
        await app.openProject("beta");
        app.model.enqueue({
          tools: [{ id: `cross-${reason}`, name: "inspect_simulator", arguments: { profileId: profile.id } }],
        });
        const crossed = app.model.enqueue({ text: "Cross-project inspection checked." });
        await app.send("Try reading a simulator bound to a different project");
        await finish(app, crossed);
        const cross = crossed.request.body.messages.find(
          (message) => message.role === "tool" && message.tool_call_id === `cross-${reason}`,
        );
        assert.match(cross.content, /Run this project/, "cancellation retains the original project boundary");
        await app.browser.switchToWindow(label);
        await app.browser.execute(() => window.__simulatorRace.release());
        const observed = await runNativeAsync(app.browser, (done) =>
          setTimeout(
            () => done({ starts: window.__simulatorRace.starts, commands: window.__simulatorRace.commands }),
            100,
          ),
        );
        assert.deepEqual(observed, { starts: 0, commands: 0 });
        await app.browser.execute(() => {
          window.__simulatorRace.readyCommands = true;
          window.__simulatorRace.acceptCommands = true;
        });
        await app.browser.switchToWindow("main");
        await app.openProject("alpha");
        await app.browser.$('button[aria-label="项目信息"]').click();
        await chooseSelect(app, "设备与模拟器运行", "allow");
        await app.browser.$('button[aria-label="关闭项目信息"]').click();
        const fresh = queueRun(app, profile.id);
        await app.send("Start a fresh authorized run after cancellation");
        await finish(app, fresh.response);
        const freshResult = fresh.response.request.body.messages.filter((message) => message.role === "tool").at(-1);
        const freshStatus = JSON.parse(freshResult.content);
        assert.notEqual(freshStatus.requestId, requestId, "a new invocation creates its own execution binding");
        assert.notEqual(freshStatus.app.status, "canceled", "cancellation does not block a fresh authorized run");
        await app.browser.switchToWindow(label);
        await waitFor(
          () => app.browser.execute(() => window.__simulatorRace.starts === 1),
          "the fresh invocation may power on",
        );
        await app.browser.switchToWindow("main");
        assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
      }
    },
    { vision: true },
  ),
);

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
    await stop(app);
    await app.browser.execute(() => window.__toolOpeningRace.release());
    await app.idle();
    await app.browser.switchToWindow(label);
    await waitFor(
      () =>
        app.browser.execute(() => window.__simulatorRace.entered && window.__simulatorRace.appReplies.some(Boolean)),
      "the successor UI request starts after the shared opening settles",
    );
    const successor = await app.browser.execute(() => window.__simulatorRace.appReplies.at(-1));
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
        const superseded = response.request.body.messages.filter((message) => message.role === "tool").at(-1);
        assert.match(
          superseded.content,
          /canceled or superseded/,
          `${boundary}: a changed target rejects the older invocation`,
        );
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
