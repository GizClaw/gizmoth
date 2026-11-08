import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import test from "node:test";
import { chooseSelect, runNativeAsync, waitFor, withApp } from "./native-app.mjs";

// Read the mounted controller only in the test driver; production uses the SDK's public API.
async function firmwareCommand(app, command) {
  return runNativeAsync(
    app.browser,
    (command, done) => {
      const host = document.querySelector(".game-preview");
      let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
      let player;
      for (; fiber && !player; fiber = fiber.return) {
        for (let hook = fiber.memoizedState; hook; hook = hook.next) {
          if (hook.memoizedState?.current?.executeCommand) player = hook.memoizedState.current;
        }
      }
      if (!player) return done({ ok: false, reason: "Controller not mounted" });
      player.executeCommand(command).then(
        (value) => done({ ok: true, value }),
        (error) => done({ ok: false, reason: String(error), code: error.code, rpcCode: error.rpcCode }),
      );
    },
    command,
  );
}

async function agentTools(app, tools) {
  await app.browser.switchToWindow("main");
  const request = app.model.enqueue({ tools });
  const response = app.model.enqueue({ text: "Simulator tool check completed." });
  await app.send("Exercise the requested local simulator tools");
  await waitFor(() => !!request.request, "agent receives simulator tools");
  await waitFor(() => !!response.request, "simulator tool results reach the model", 60000);
  await app.idle();
  return Object.fromEntries(
    response.request.body.messages.filter((m) => m.role === "tool").map((m) => [m.tool_call_id, m.content]),
  );
}

const withVisionApp = (t, run) => withApp(t, run, { vision: true });

// No host clicks in the simulator: firmware startup, capture, and input all come from agent tools.
test(
  "agent boots a new simulator and reads and controls its real screen",
  {
    skip: process.env.GIZMOTH_LIVE_SIMULATOR_TEST !== "1",
    timeout: 180000,
  },
  (t) =>
    withVisionApp(t, async (app) => {
      await app.openProject("alpha");
      await app.browser.$('button[aria-label="项目信息"]').click();
      await chooseSelect(app, "设备与模拟器运行", "allow");
      await app.browser.$('button[aria-label="关闭项目信息"]').click();
      let started = await agentTools(app, [{ id: "auto-start", name: "run_in_simulator", arguments: {} }]);
      const profiles = (await app.invoke("simulator_profiles_list")).value;
      assert.equal(profiles.length, 1);
      const label = `simulator-${profiles[0].id.replaceAll("-", "")}`;
      // A slow provider catalog rejects startup before any write; wait for discovery, then retry only that case.
      if (started["auto-start"] === "Simulator firmware catalog is still loading. Inspect before retrying.") {
        await app.browser.switchToWindow(label);
        await app.browser.$("#preview-firmware-panel button.primary").waitForEnabled({ timeout: 90000 });
        started = await agentTools(app, [
          { id: "auto-start", name: "run_in_simulator", arguments: { profileId: profiles[0].id } },
        ]);
      }
      const status = JSON.parse(started["auto-start"]);
      assert.equal(status.error, null);
      await app.browser.switchToWindow(label);
      const state = () => app.browser.execute(() => document.querySelector("iframe")?.contentWindow?.h2WebState ?? {});
      await waitFor(
        async () => (await app.browser.$(".preview-state").getText()).includes("试玩环境运行中"),
        "agent boots firmware without a Power click",
        90000,
      );
      await waitFor(
        async () => {
          const value = await state();
          return value.tutorialActive && value.tutorialAudioReady && !value.tutorialAudioBusy;
        },
        "first-boot key lesson",
        30000,
      );
      const results = await agentTools(app, [
        { id: "auto-screen", name: "inspect_simulator", arguments: { capture: true } },
        { id: "auto-input", name: "press_simulator_button", arguments: { button: "back", durationMs: 140 } },
      ]);
      const snapshot = JSON.parse(results["auto-screen"]);
      assert.equal(snapshot.phase, "running");
      assert.equal(snapshot.screen.width, 240);
      assert.match(snapshot.screen.hash, /^[0-9a-f]{8}$/);
      await app.browser.switchToWindow(label);
      await waitFor(async () => (await state()).tutorialKeyStep === 1, "agent button reaches firmware tutorial", 10000);
      await app.screenshot("simulator-agent-startup-native");

      // Both host and SDK focus handlers run while a real agent-owned hold is active.
      await app.browser.execute(() => {
        const host = document.querySelector(".game-preview");
        let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
        let player;
        for (; fiber && !player; fiber = fiber.return) {
          for (let hook = fiber.memoizedState; hook; hook = hook.next) {
            if (hook.memoizedState?.current?.executeCommand) player = hook.memoizedState.current;
          }
        }
        const buttons = player.viewer.buttons;
        const probe = (window.__agentFocusHold = { buttons, events: [] });
        buttons.subscribe((event) => {
          if (event.button === "volume_up") probe.events.push({ pressed: event.pressed, time: performance.now() });
        });
      });
      await app.browser.switchToWindow("main");
      const holdRequest = app.model.enqueue({
        tools: [
          { id: "focus-hold", name: "press_simulator_button", arguments: { button: "volume_up", durationMs: 2500 } },
        ],
      });
      const holdResponse = app.model.enqueue({ text: "Hold completed after focus cleanup." });
      await app.send("Hold a simulator button while focus changes");
      await waitFor(() => !!holdRequest.request, "agent starts the hold command");
      await app.browser.switchToWindow(label);
      await waitFor(
        () => app.browser.execute(() => window.__agentFocusHold.events.some((event) => event.pressed)),
        "real SDK button is held",
      );
      assert.equal(
        await app.browser.execute(() => {
          document
            .querySelector(".preview-play-area")
            .dispatchEvent(new FocusEvent("focusout", { bubbles: true, relatedTarget: document.body }));
          window.dispatchEvent(new Event("blur"));
          const previous = Object.getOwnPropertyDescriptor(document, "hidden");
          Object.defineProperty(document, "hidden", { configurable: true, value: true });
          document.dispatchEvent(new Event("visibilitychange"));
          if (previous) Object.defineProperty(document, "hidden", previous);
          else delete document.hidden;
          return window.__agentFocusHold.buttons.getPressed().includes("volume_up");
        }),
        true,
        "focusout, blur and visibility cleanup cannot release the agent source",
      );
      await app.browser.switchToWindow("main");
      await waitFor(() => !!holdResponse.request, "agent hold finishes normally");
      await app.idle();
      await app.browser.switchToWindow(label);
      const edges = await app.browser.execute(() => window.__agentFocusHold.events);
      assert.deepEqual(
        edges.map((event) => event.pressed),
        [true, false],
      );
      assert.ok(edges[1].time - edges[0].time >= 2300, "real button remains held for the requested duration");

      // A late command failure must remain inspectable after its UI notice is dismissed.
      assert.equal(
        await app.browser.execute(() => {
          const host = document.querySelector(".game-preview");
          let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
          let player;
          for (; fiber && !player; fiber = fiber.return) {
            for (let hook = fiber.memoizedState; hook; hook = hook.next) {
              if (hook.memoizedState?.current?.executeCommand) player = hook.memoizedState.current;
            }
          }
          if (!player) return false;
          const failure = (window.__dismissedAppFailure = { held: false, fail: null });
          const execute = player.executeCommand.bind(player);
          let armed = true;
          player.executeCommand = (command, options) => {
            if (armed && command.method === "tools.invoke" && command.tool === "lua.app.run") {
              armed = false;
              failure.held = true;
              return new Promise((resolve, reject) => {
                failure.fail = () => {
                  failure.held = false;
                  reject(Object.assign(new Error("Injected late launch failure"), { code: -3 }));
                };
              });
            }
            return execute(command, options);
          };
          return true;
        }),
        true,
      );
      await waitFor(
        () => app.browser.execute(() => window.__dismissedAppFailure.held),
        "launch command is pending before dismissal",
        30000,
      );
      await app.browser.$("button=关闭提示").click();
      await app.browser.execute(() => window.__dismissedAppFailure.fail());
      const failed = await agentTools(app, [
        { id: "dismissed-failure", name: "inspect_simulator", arguments: { waitMs: 100 } },
      ]);
      const failureStatus = JSON.parse(failed["dismissed-failure"]);
      assert.equal(failureStatus.app.status, "error");
      assert.match(failureStatus.app.error, /当前固件不支持此操作/);
      await app.browser.switchToWindow(label);
      assert.equal(
        await app.browser.$(".preview-notes").isExisting(),
        false,
        "dismissed notice stays hidden while diagnostics survive",
      );

      // Hold the tail of the real shutdown promise after it has published "stopped".
      // Reload still owns the task lock while this models pending persistence work.
      await app.browser.$("button.preview-firmware-toggle").click();
      await app.browser.$('button[aria-label="重新加载预览"]').waitForEnabled();
      assert.equal(
        await app.browser.execute(() => {
          const host = document.querySelector(".game-preview");
          let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
          let player;
          for (; fiber && !player; fiber = fiber.return) {
            for (let hook = fiber.memoizedState; hook; hook = hook.next) {
              if (hook.memoizedState?.current?.executeCommand) player = hook.memoizedState.current;
            }
          }
          if (!player) return false;
          const race = (window.__agentShutdownRace = {
            held: false,
            destroyedWhileHeld: false,
            phase: null,
            release: null,
          });
          const shutdown = player.shutdown.bind(player);
          const destroy = player.destroy.bind(player);
          player.shutdown = async () => {
            await shutdown();
            race.phase = player.state.phase;
            race.held = true;
            await new Promise((resolve) => {
              race.release = () => {
                race.held = false;
                resolve();
              };
            });
          };
          player.destroy = () => {
            if (race.held) race.destroyedWhileHeld = true;
            destroy();
          };
          return true;
        }),
        true,
      );
      await app.browser.$('button[aria-label="重新加载预览"]').click();
      await waitFor(
        () => app.browser.execute(() => window.__agentShutdownRace.held),
        "Reload holds its shutdown barrier",
        30000,
      );
      assert.equal(await app.browser.execute(() => window.__agentShutdownRace.phase), "stopped");
      const blocked = await agentTools(app, [
        { id: "shutdown-race", name: "run_in_simulator", arguments: { profileId: profiles[0].id } },
      ]);
      assert.match(blocked["shutdown-race"], /busy saving or changing firmware/);
      await app.browser.switchToWindow(label);
      assert.deepEqual(
        await app.browser.execute(() => ({
          held: window.__agentShutdownRace.held,
          destroyed: window.__agentShutdownRace.destroyedWhileHeld,
        })),
        { held: true, destroyed: false },
      );
      await app.browser.execute(() => window.__agentShutdownRace.release());
      await waitFor(
        async () => /等待启动|正在启动|试玩环境运行中/.test(await app.browser.$(".preview-state").getText()),
        "Reload replaces the player after persistence settles",
        90000,
      );
      assert.equal(await app.browser.execute(() => window.__agentShutdownRace.destroyedWhileHeld), false);
      await app.screenshot("simulator-agent-shutdown-barrier-native");
      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_profile_delete", { id: profiles[0].id })).ok, true);

      // Failed recovery must preserve local intent, so a later legacy retry cannot target hardware.
      const manifest = await app.invoke("project_read", { id: "alpha", path: "app.json" });
      assert.equal(manifest.ok, true);
      assert.equal((await app.invoke("project_write", { id: "alpha", path: "app.json", content: "{}" })).ok, true);
      const rejected = await agentTools(app, [
        { id: "invalid-recovery", name: "run_in_simulator", arguments: {} },
        { id: "preserved-route", name: "list_simulators", arguments: {} },
      ]);
      assert.match(rejected["invalid-recovery"], /app_id|app.json/);
      const retained = JSON.parse(rejected["preserved-route"]);
      assert.equal(retained.selected, profiles[0].id, "failed validation retains simulator routing intent");
      assert.deepEqual(retained.simulators, [], "failed validation cannot create a replacement");
      assert.equal(
        (await app.invoke("project_write", { id: "alpha", path: "app.json", content: manifest.value })).ok,
        true,
      );

      // Deleting the last selected simulator must not strand either assistant run tool.
      const recovered = await agentTools(app, [
        { id: "explicit-deleted", name: "run_in_simulator", arguments: { profileId: profiles[0].id } },
        { id: "recover-legacy", name: "push_to_device", arguments: {} },
        { id: "recovered-target", name: "list_simulators", arguments: {} },
      ]);
      assert.match(recovered["explicit-deleted"], /Simulator does not exist/);
      const targets = JSON.parse(recovered["recovered-target"]);
      assert.equal(targets.simulators.length, 1, "a missing implicit target permits simulator creation");
      assert.notEqual(targets.selected, profiles[0].id);
      assert.equal(targets.selected, targets.simulators[0].id);
      // Catalog loading rejects before startup is submitted; any other result must be a real status snapshot.
      if (recovered["recover-legacy"] !== "Simulator firmware catalog is still loading. Inspect before retrying.") {
        const restarted = JSON.parse(recovered["recover-legacy"]);
        assert.equal(restarted.profileId, targets.selected);
        assert.equal(restarted.error, null);
      }
      assert.equal((await app.invoke("simulator_profile_delete", { id: targets.selected })).ok, true);
    }),
);

// Opt in: exercises recovery with real Trial firmware, controlling only failure timing.
test(
  "native simulator Run recovers from canceled installation and a stuck startup",
  { skip: process.env.GIZMOTH_LIVE_SIMULATOR_TEST !== "1", timeout: 180000 },
  (t) =>
    withApp(t, async (app) => {
      for (const failure of ["canceled-install", "startup-deadline", "manager-clear"]) {
        const profile = {
          id: randomUUID(),
          name: `Recovery ${failure}`,
          model: "h106-tiga",
          firmwareId: "",
          view: "screen",
        };
        const label = `simulator-${profile.id.replaceAll("-", "")}`;
        assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
        // Use the manager's ordinary opener before Run so its session token and
        // mounted controller survive delivery of the incoming project request.
        await app.browser.$('button[data-page="projects"]').click();
        await app.browser.$('button[data-page="simulators"]').click();
        await app.browser.$(`button[aria-label="打开 ${profile.name}"]`).click();
        await waitFor(async () => (await app.browser.getWindowHandles()).includes(label), "recovery simulator opens");
        await app.browser.switchToWindow(label);
        await app.browser.$(".game-preview").waitForDisplayed({ timeout: 30000 });
        assert.equal(await app.browser.$(".preview-incoming-app").isExisting(), false);
        await waitFor(
          () =>
            runNativeAsync(
              app.browser,
              async (failure, done) => {
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
                const probe = (window.__simulatorRecovery = {
                  player,
                  installed: false,
                  blockedStarts: 0,
                  enterCalls: 0,
                  startCalls: 0,
                });
                const start = player.start;
                player.start = function (...args) {
                  probe.startCalls++;
                  return start.apply(this, args);
                };
                const enter = player.enter;
                player.enter = async function (...args) {
                  probe.enterCalls++;
                  await enter.apply(this, args);
                  probe.frame = document.querySelector("iframe");
                  probe.installedPhase = player.state.phase;
                  if (failure === "canceled-install") {
                    await new Promise((resolve) => {
                      probe.release = resolve;
                      probe.installed = true;
                    });
                    probe.released = true;
                  } else {
                    // The SDK's real start() clicks this button. Suppress only
                    // delivery to app_host; leave SDK phase and WASM state real.
                    const child = probe.frame.contentWindow;
                    probe.childNow = child.Date.now;
                    probe.blockStart = (event) => {
                      if (!event.target.closest("button#start")) return;
                      event.preventDefault();
                      event.stopImmediatePropagation();
                      probe.blockedStarts++;
                    };
                    child.document.addEventListener("click", probe.blockStart, true);
                    probe.installed = true;
                  }
                };
                done(true);
              },
              failure,
            ),
          "SDK controller is ready and real enter is wrapped",
          30000,
        );
        await app.browser.switchToWindow("main");
        await app.browser.$('button[data-page="projects"]').click();
        await app.openProject("alpha");
        await app.browser.$('button[aria-label="运行"]').click();
        await app.browser.$(".run-target-row").waitForDisplayed();
        // The target picker lists both profiles on the second pass.
        const rows = await app.browser.$$(".run-target-row");
        const names = await rows.map((row) => row.getText());
        const index = names.findIndex((name) => name.includes(profile.name));
        assert.ok(index >= 0, `Run target exists: ${profile.name}`);
        await rows[index].click();
        await app.browser.switchToWindow(label);
        await waitFor(
          () => app.browser.execute(() => window.__simulatorRecovery?.installed === true),
          "real firmware installation completes in the original manager window",
          90000,
        );
        assert.equal(await app.browser.execute(() => window.__simulatorRecovery.installedPhase), "ready");
        if (process.env.GIZMOTH_DISABLE_RTC_TRANSFORM_TEST === "1")
          assert.equal(
            await app.browser.execute(
              () => typeof document.querySelector("iframe").contentWindow.RTCRtpScriptTransform,
            ),
            "undefined",
          );
        try {
          if (failure === "canceled-install") {
            assert.ok((await app.browser.$(".preview-state").getText()).includes("等待启动"));
            await app.browser.$("button=停止等待").click();
            // Stop must release the UI from the still-unresolved enter() wait.
            const retry = await app.browser.$("button=重试安装");
            await retry.waitForEnabled();
            assert.equal(await app.browser.execute(() => window.__simulatorRecovery.released === true), false);
            assert.ok((await app.browser.$(".preview-state").getText()).includes("等待启动"));
            await retry.click();
            await app.browser.$("button=停止等待").waitForEnabled();
            assert.deepEqual(
              await app.browser.execute(() => {
                const probe = window.__simulatorRecovery;
                return {
                  released: probe.released === true,
                  phase: probe.player.state.phase,
                  enterCalls: probe.enterCalls,
                  startCalls: probe.startCalls,
                };
              }),
              { released: false, phase: "ready", enterCalls: 1, startCalls: 0 },
              "Retry joins the in-flight installation without entering again or starting early",
            );
            await app.browser.execute(() => window.__simulatorRecovery.release());
            await waitFor(
              () => app.browser.execute(() => window.__simulatorRecovery.released === true),
              "original enter wrapper finishes before Retry can boot",
            );
          } else {
            await waitFor(
              () =>
                app.browser.execute(() => {
                  const probe = window.__simulatorRecovery;
                  return probe.blockedStarts > 0 && probe.player.state.phase === "starting";
                }),
              "real SDK start is pending after app_host click is withheld",
            );
            assert.ok((await app.browser.$(".preview-state").getText()).includes("正在启动"));
            if (failure === "manager-clear") {
              await app.browser.switchToWindow("main");
              await app.browser.$('button[data-page="projects"]').click();
              await app.browser.$('button[data-page="simulators"]').click();
              await app.browser.$(`button[aria-label="打开 ${profile.name}"]`).click();
              await app.browser.switchToWindow(label);
              await waitFor(
                () =>
                  app.browser
                    .$(".preview-incoming-app")
                    .isExisting()
                    .then((exists) => !exists),
                "manager clears the incoming Run",
              );
              assert.equal(await app.browser.execute(() => window.__simulatorRecovery.player.state.phase), "starting");
              await app.browser.$("button.preview-firmware-toggle").click();
              const reload = await app.browser.$('button[aria-label="重新加载预览"]');
              await reload.waitForEnabled();
              await reload.click();
              await app.browser.$('button[aria-label="关闭固件设置"]').click();
              await waitFor(
                () => app.browser.execute(() => window.__simulatorRecovery.player.state.phase === "destroyed"),
                "manager-cleared startup can be reloaded",
              );
              await app.browser.switchToWindow("main");
              await app.browser.$('button[data-page="projects"]').click();
              await app.openProject("alpha");
              await app.browser.$('button[aria-label="运行"]').click();
              await app.browser.$(".run-target-row").waitForDisplayed();
              const managerRows = await app.browser.$$(".run-target-row");
              const managerNames = await managerRows.map((row) => row.getText());
              await managerRows[managerNames.findIndex((name) => name.includes(profile.name))].click();
              await app.browser.switchToWindow(label);
            } else {
              await app.browser.execute(() => {
                const probe = window.__simulatorRecovery;
                probe.readSignal = () => {
                  const host = document.querySelector(".game-preview");
                  let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
                  for (; fiber; fiber = fiber.return)
                    for (let hook = fiber.memoizedState; hook; hook = hook.next) {
                      const value = hook.memoizedState?.current;
                      if (value?.signal && typeof value.abort === "function") return value.signal;
                    }
                  return null;
                };
                probe.previousSignal = probe.readSignal();
              });
              await app.browser.switchToWindow("main");
              await app.browser.$('button[aria-label="运行"]').click();
              await app.browser.$(".run-target-row").waitForDisplayed();
              const nextRows = await app.browser.$$(".run-target-row");
              const nextNames = await nextRows.map((row) => row.getText());
              await nextRows[nextNames.findIndex((name) => name.includes(profile.name))].click();
              await app.browser.switchToWindow(label);
              await waitFor(
                () =>
                  app.browser.execute(() => {
                    const probe = window.__simulatorRecovery;
                    const signal = probe.readSignal();
                    return (
                      probe.previousSignal?.aborted && signal && signal !== probe.previousSignal && !signal.aborted
                    );
                  }),
                "a replacement Run joins startup with its own bounded request",
              );
              assert.equal(await app.browser.execute(() => window.__simulatorRecovery.blockedStarts), 1);
              await app.browser.execute(() => {
                const probe = window.__simulatorRecovery;
                probe.now = Date.now;
                Date.now = () => probe.now() + 61000;
              });
              await waitFor(
                () =>
                  app.browser.execute(
                    () =>
                      document.querySelector(".preview-notes .error")?.textContent?.includes("固件启动超时") ?? false,
                  ),
                "startup deadline is shown in the actual UI",
              );
              assert.equal(
                await app.browser.execute(() => {
                  const probe = window.__simulatorRecovery;
                  return (
                    probe.player.state.phase === "starting" && probe.frame.contentWindow.Date.now === probe.childNow
                  );
                }),
                true,
                "SDK remains starting and the firmware clock is unchanged",
              );
            }
          }
        } finally {
          await app.browser.execute(() => {
            const probe = window.__simulatorRecovery;
            if (probe.now) Date.now = probe.now;
            probe.release?.();
            if (probe.blockStart)
              probe.frame.contentWindow?.document.removeEventListener("click", probe.blockStart, true);
          });
        }
        if (failure === "startup-deadline") {
          const retry = await app.browser.$("button=重试安装");
          await retry.waitForEnabled();
          await app.browser.$("button.preview-firmware-toggle").click();
          assert.equal(await app.browser.$('button[aria-label="重新加载预览"]').isEnabled(), true);
          await app.screenshot("simulator-startup-timeout-recovery-native");
          await app.browser.$('button[aria-label="关闭固件设置"]').click();
          await retry.click();
        }
        // No physical Power press: Retry itself must boot actual firmware.
        await waitFor(
          async () => (await app.browser.$(".preview-state").getText()).includes("试玩环境运行中"),
          "Retry boots real firmware without Power",
          45000,
        );
        if (process.env.GIZMOTH_DISABLE_RTC_TRANSFORM_TEST === "1")
          assert.equal(
            await app.browser.execute(
              () => typeof document.querySelector("iframe").contentWindow.RTCRtpScriptTransform,
            ),
            "undefined",
          );
        assert.equal(
          await app.browser.execute((failure) => {
            const probe = window.__simulatorRecovery;
            const frame = document.querySelector("iframe");
            return failure === "canceled-install"
              ? frame === probe.frame && probe.player.state.phase === "running"
              : frame !== probe.frame && !probe.frame.isConnected && probe.player.state.phase === "destroyed";
          }, failure),
          true,
          "canceled ready runtime is reused; stuck starting runtime is recreated",
        );
        await waitFor(
          () =>
            app.browser.execute(
              () => document.querySelector("iframe")?.contentWindow?.h2WebState?.tutorialActive === true,
            ),
          "recovered firmware really enters first-boot setup",
          30000,
        );
        let commands;
        await waitFor(
          async () => {
            commands = await firmwareCommand(app, { method: "tools.list" });
            return commands.ok;
          },
          "recovered firmware accepts real commands",
          30000,
        );
        assert.ok(commands.value.some((tool) => tool.name === "lua.app.install"));
        // Release the app waiter while retaining the incoming request, so normal
        // Reload must wait for its new controller and automatically boot it.
        await app.browser.$("button=停止等待").click();
        await waitFor(
          () =>
            app.browser.execute(() =>
              [...document.querySelectorAll(".preview-incoming-app button")].some(
                (button) => /重试(?:安装|运行)/.test(button.textContent) && !button.disabled,
              ),
            ),
          "recovered request stops waiting",
        );
        if (failure === "canceled-install") {
          assert.equal(await app.browser.$(".preview-incoming-app").isDisplayed(), true);
          await app.browser.execute(() => {
            const probe = window.__simulatorRecovery;
            probe.readPlayer = () => {
              const host = document.querySelector(".game-preview");
              let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
              for (; fiber; fiber = fiber.return)
                for (let hook = fiber.memoizedState; hook; hook = hook.next) {
                  const value = hook.memoizedState?.current;
                  if (value?.executeCommand) return value;
                }
              return null;
            };
            probe.beforeReload = probe.readPlayer();
            probe.beforeReloadFrame = document.querySelector("iframe");
          });
          await app.browser.$("button.preview-firmware-toggle").click();
          const reload = await app.browser.$('button[aria-label="重新加载预览"]');
          await reload.waitForEnabled();
          await reload.click();
          await app.browser.$('button[aria-label="关闭固件设置"]').click();
          await waitFor(
            () =>
              app.browser.execute(() => {
                const probe = window.__simulatorRecovery;
                const player = probe.readPlayer();
                const frame = document.querySelector("iframe");
                return (
                  player &&
                  player !== probe.beforeReload &&
                  player.state.phase === "running" &&
                  frame &&
                  frame !== probe.beforeReloadFrame &&
                  !probe.beforeReloadFrame.isConnected &&
                  probe.beforeReload.state.phase === "destroyed" &&
                  document.querySelector(".preview-state")?.textContent?.includes("试玩环境运行中")
                );
              }),
            "normal Reload automatically boots the replacement controller without Power",
            45000,
          );
          await waitFor(
            () =>
              app.browser.execute(
                () => document.querySelector("iframe")?.contentWindow?.h2WebState?.tutorialActive === true,
              ),
            "normally reloaded firmware resumes real first-boot setup",
            30000,
          );
          await waitFor(
            async () => {
              commands = await firmwareCommand(app, { method: "tools.list" });
              return commands.ok;
            },
            "normally reloaded firmware accepts real commands",
            30000,
          );
          assert.ok(commands.value.some((tool) => tool.name === "lua.app.install"));
          await app.browser.$("button=停止等待").click();
          await waitFor(
            () =>
              app.browser.execute(() =>
                [...document.querySelectorAll(".preview-incoming-app button")].some(
                  (button) => /重试(?:安装|运行)/.test(button.textContent) && !button.disabled,
                ),
              ),
            "reloaded request stops waiting before closing",
          );
        }
        await app.browser.closeWindow();
        await waitFor(async () => !(await app.browser.getWindowHandles()).includes(label), "recovery simulator closes");
        await app.browser.switchToWindow("main");
      }
    }),
);

// Opt in: downloads the provider's real Trial firmware and uses isolated native data stores.
test(
  "native simulators retain their own device data across restart and delete independently",
  { skip: process.env.GIZMOTH_LIVE_SIMULATOR_TEST !== "1", timeout: 420000 },
  (t) =>
    withVisionApp(t, async (app) => {
      const source = `local display = require("display")
local runtime = require("runtime")
print("GIZMOTH_SIMULATOR_PROBE_RUNNING")
while true do
  display.clear("green")
  display.draw_text(28, 100, "LUA OK", { color = "white", font_size = 16 })
  display.present()
  runtime.sleep(50)
end
`;
      const profile = { id: randomUUID(), name: "Fixed Tiga A", model: "h106-tiga", firmwareId: "", view: "screen" };
      const second = { ...profile, id: randomUUID(), name: "Fixed Tiga B" };
      const label = (profile) => `simulator-${profile.id.replaceAll("-", "")}`;
      const state = () => app.browser.execute(() => document.querySelector("iframe")?.contentWindow?.h2WebState ?? {});
      const press = async (name, hold = 140) => {
        const button = await app.browser.$(`.game-key[aria-label$="（${name}）"]`);
        await app.browser.action("pointer").move({ origin: button }).down().pause(hold).up().perform();
      };
      const start = async (installFirmware, automatic = false) => {
        await app.browser
          .$(".game-preview")
          .waitForDisplayed()
          .catch(async (error) => {
            console.error(
              JSON.stringify(
                await runNativeAsync(app.browser, (done) => {
                  const scripts = [...document.scripts].map((script) => script.src).filter(Boolean);
                  Promise.all(
                    scripts.map(async (url) => {
                      const response = await fetch(url);
                      let imported;
                      try {
                        await import(url);
                        imported = "ok";
                      } catch (error) {
                        imported = String(error) + " " + error.stack;
                      }
                      return { url, status: response.status, imported };
                    }),
                  ).then((scripts) =>
                    done({
                      url: location.href,
                      scripts,
                      resources: performance.getEntriesByType("resource").map((entry) => entry.name),
                    }),
                  );
                }),
              ),
            );
            throw error;
          });
        assert.equal(await app.browser.$('[aria-label="预览机型"]').isExisting(), false);
        assert.equal(await app.browser.$(".preview-model-label").getText(), "H106 Tiga");
        assert.ok(!(await app.browser.$(".preview-runtime").getText()).includes("停止"));
        if (installFirmware) {
          await app.browser.$("button.preview-firmware-toggle").click();
          const install = await app.browser.$("#preview-firmware-panel button.primary");
          await install.waitForEnabled({ timeout: 30000 });
          await install.click();
        }
        if (automatic) {
          await waitFor(
            () => app.browser.execute(() => !!document.querySelector("iframe")?.contentWindow?.Module),
            "Run installs firmware without opening the firmware picker",
            90000,
          );
        } else {
          await waitFor(
            async () => (await app.browser.$(".preview-state").getText()).includes("等待启动"),
            "firmware becomes ready",
            90000,
          );
        }
        if (installFirmware) await app.browser.$('button[aria-label="关闭固件设置"]').click();
        await app.browser.execute(() => {
          const child = document.querySelector("iframe").contentWindow;
          const module = child.Module;
          window.__simulatorTrace = [];
          const record = (line) => {
            if (/H2_H106_|LUA|GIZCLAW|gizclaw|GizClaw|local_control|TUTORIAL|rpc|app=|BOOT_NETWORK/.test(line))
              window.__simulatorTrace.push(line);
          };
          for (const key of ["log", "info", "warn", "error", "debug"]) {
            const original = child.console[key];
            child.console[key] = (...args) => {
              record(args.join(" "));
              original.apply(child.console, args);
            };
          }
          const policy = (event) =>
            window.__simulatorTrace.push(
              JSON.stringify({
                event: "security-policy",
                blocked: event.blockedURI,
                directive: event.violatedDirective,
              }),
            );
          child.document.addEventListener("securitypolicyviolation", policy);
          document.addEventListener("securitypolicyviolation", policy);
          let api = module.h2GizClaw;
          Object.defineProperty(module, "h2GizClaw", {
            configurable: true,
            enumerable: true,
            get: () => api,
            set: (next) => {
              window.__simulatorTrace.push(
                JSON.stringify({
                  event: "control-interface",
                  available: !!next,
                  state: child.h2WebState,
                  stack: new child.Error().stack,
                }),
              );
              api = next;
            },
          });
          child.addEventListener("pagehide", (event) =>
            window.__simulatorTrace.push(JSON.stringify({ event: "pagehide", persisted: event.persisted })),
          );
          for (const key of ["print", "printErr"]) {
            const original = module[key];
            module[key] = (...args) => {
              const line = args.join(" ");
              if (/LUA|GIZCLAW|GizClaw|local_control|TUTORIAL|rpc|app=|BOOT_NETWORK/.test(line))
                window.__simulatorTrace.push(line);
              original?.apply(module, args);
            };
          }
        });
        // The native fixture applies the optional capability boundary at document
        // start for all firmware frames, including automatically booted replacements.
        if (process.env.GIZMOTH_DISABLE_RTC_TRANSFORM_TEST === "1")
          assert.equal(
            await app.browser.execute(
              () => typeof document.querySelector("iframe").contentWindow.RTCRtpScriptTransform,
            ),
            "undefined",
          );
        const accessPoint = await runNativeAsync(app.browser, (done) => {
          const child = document.querySelector("iframe").contentWindow;
          child.fetch("https://ap.trial.gizclaw.com/server-info", { signal: child.AbortSignal.timeout(5000) }).then(
            (response) => done({ ok: response.ok, status: response.status }),
            (error) => done({ ok: false, reason: String(error) }),
          );
        });
        assert.equal(
          accessPoint.ok,
          true,
          `Trial /server-info must be reachable from the actual firmware frame: ${JSON.stringify(accessPoint)}`,
        );
        if (!automatic) await press("power");
        const windowLabel = await app.browser.getWindowHandle();
        if (process.env.GIZMOTH_SIMULATOR_BACKGROUND_TEST === "1")
          assert.equal((await app.invoke("plugin:window|set_focus", { label: "main" })).ok, true);
        await waitFor(
          () =>
            app.browser.execute(
              () => document.querySelector(".preview-state")?.textContent?.includes("试玩环境运行中") ?? false,
            ),
          "firmware reaches running even while the editor has focus",
          30000,
        );
        if (process.env.GIZMOTH_SIMULATOR_BACKGROUND_TEST === "1")
          assert.equal((await app.invoke("plugin:window|set_focus", { label: windowLabel })).ok, true);
        await waitFor(async () => (await state()).luaCatalogReady, "firmware catalog is ready", 30000);
      };
      const openFromMain = async (profile) => {
        await app.browser.switchToWindow("main");
        await app.browser.$('button[data-page="projects"]').click();
        await app.browser.$('button[data-page="simulators"]').click();
        const open = await app.browser.$(`button[aria-label="打开 ${profile.name}"]`);
        await open.waitForDisplayed();
        await open.click();
        await waitFor(async () => (await app.browser.getWindowHandles()).includes(label(profile)), "simulator opens");
        await app.browser.switchToWindow(label(profile));
        // A native handle exists before its initial about:blank navigation ends.
        await app.browser.$(".game-preview").waitForDisplayed({ timeout: 30000 });
      };
      const apps = async () => {
        let response;
        await waitFor(
          async () => {
            response = await firmwareCommand(app, { method: "tools.invoke", tool: "lua.app.list", args: {} });
            return response.ok;
          },
          "firmware command interface becomes ready",
          60000,
        );
        return response.value.apps;
      };
      const deleteCurrent = async (profile) => {
        const remove = await app.browser.$(".simulator-titlebar .simulator-delete-text");
        await remove.waitForEnabled();
        await remove.click();
        await app.browser.$(".dialog-actions button.danger").click();
        await waitFor(
          async () => !(await app.browser.getWindowHandles()).includes(label(profile)),
          "deleted simulator window closes",
        );
        await app.browser.switchToWindow("main");
        await waitFor(
          async () => !(await app.invoke("simulator_profiles_list")).value.some((p) => p.id === profile.id),
          "native device data deletion completes",
          45000,
        );
      };

      assert.equal((await app.invoke("project_write", { id: "alpha", path: "alpha.lua", content: source })).ok, true);
      assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
      assert.equal(
        (await app.invoke("simulator_profile_update", { id: profile.id, patch: { model: "h106-zero" } })).ok,
        false,
      );
      await app.openProject("alpha");
      await app.browser.$('button[aria-label="运行"]').click();
      await app.browser.$(".run-target-row").waitForDisplayed();
      await app.browser.$(".run-target-row").click();
      await waitFor(
        async () => (await app.browser.getWindowHandles()).includes(label(profile)),
        "native simulator opens",
      );
      await app.browser.switchToWindow(label(profile));
      assert.equal(await app.browser.$(".simulator-window > .banner.error").isExisting(), false);
      // Match the user's first Run on a fresh simulator: no firmware-panel
      // installation and no physical Power press may be needed to boot it.
      await start(false, true);
      const origin = await app.browser.execute(() => location.origin);
      await waitFor(async () => (await state()).tutorialActive, "first boot enters setup", 30000);
      await waitFor(
        () =>
          app.browser.execute(
            () => document.querySelector(".preview-notes")?.textContent?.includes("正在等待设备空闲") ?? false,
          ),
        "BUSY waits instead of failing",
        30000,
      );
      const buttons = await app.browser.$$(".game-key");
      assert.equal(buttons.length, 8);
      for (const button of buttons) assert.equal(await button.isEnabled(), true);
      await app.screenshot("simulator-waiting-for-setup-native");
      await app.browser.$("button=停止等待").click();
      await app.browser.$("button=重试运行").waitForEnabled();
      await app.browser.$("button=重试运行").click();
      await waitFor(
        () =>
          app.browser.execute(
            () => document.querySelector(".preview-notes")?.textContent?.includes("正在等待设备空闲") ?? false,
          ),
        "run-only retry resumes",
      );

      const keys = ["back", "ok", "left", "right", "volume_down", "volume_up", "record"];
      for (let step = 0; step < keys.length; step++) {
        await waitFor(
          async () => {
            const value = await state();
            return (
              value.tutorialPage === 0 &&
              value.tutorialKeyStep === step &&
              value.tutorialAudioReady &&
              !value.tutorialAudioBusy
            );
          },
          `setup key lesson ${step}`,
          20000,
        );
        await press(keys[step]);
      }
      await waitFor(
        async () => {
          const value = await state();
          return value.tutorialPage === 1 && !value.tutorialAudioBusy;
        },
        "light-core lesson",
        20000,
      );
      await press("ok");
      await waitFor(async () => (await state()).tutorialPage === 2, "record practice");
      await press("record", 1700);
      await waitFor(
        async () => {
          const value = await state();
          return value.tutorialPage === 4 && value.tutorialAudioReady && !value.tutorialAudioBusy;
        },
        "setup completion",
        30000,
      );
      await press("ok");
      await waitFor(async () => (await state()).luaActive, "queued Lua app really starts", 90000).catch(
        async (error) => {
          console.error(
            JSON.stringify(
              await app.browser.execute(() => {
                const child = document.querySelector("iframe")?.contentWindow;
                return {
                  state: child?.h2WebState,
                  closed: child?.Module?.h2GizClaw?.closed,
                  trace: window.__simulatorTrace,
                  text: document.querySelector(".preview-notes")?.innerText,
                };
              }),
            ),
          );
          throw error;
        },
      );
      const expectLuaScreen = async () => {
        // The released firmware may sleep its backlight during network teardown.
        // Wake through its real power key before checking visible output.
        if (
          await app.browser.execute(
            () => document.querySelector("iframe").contentWindow.Module.h2WebOutputs?.display?.brightnessPercent === 0,
          )
        )
          await press("power");
        await waitFor(
          () =>
            app.browser.execute(() => {
              const pixel = document
                .querySelector("canvas.preview-screen")
                .getContext("2d")
                .getImageData(10, 10, 1, 1).data;
              return pixel[1] > 50 && pixel[0] < 20 && pixel[2] < 20;
            }),
          "Lua framebuffer reaches the screen",
        );
      };
      await expectLuaScreen();
      assert.equal(await app.browser.$(".preview-notes .error").isExisting(), false);
      await app.browser.$("button=关闭提示").click();
      await app.screenshot("simulator-fixed-device-running-native");

      // Drive actual button input through the model -> agent tool -> window channel.
      await app.browser.switchToWindow("main");
      await app.browser.$('button[aria-label="项目信息"]').click();
      await chooseSelect(app, "设备与模拟器运行", "allow");
      await app.browser.$('button[aria-label="关闭项目信息"]').click();
      let results = await agentTools(app, [
        { id: "inspect-running", name: "inspect_simulator", arguments: { capture: true } },
        { id: "exit-with-agent", name: "press_simulator_button", arguments: { button: "back", durationMs: 1600 } },
      ]);
      const status = JSON.parse(results["inspect-running"]);
      assert.equal(status.app.status, "run-requested");
      assert.equal(status.app.appId, "alpha");
      assert.equal(status.screen.width, 240);
      assert.ok(
        app.model.requests
          .at(-1)
          .body.messages.some(
            (message) =>
              message.role === "user" &&
              Array.isArray(message.content) &&
              message.content.some(
                (block) => block.type === "image_url" && block.image_url.url.startsWith("data:image/png;base64,"),
              ),
          ),
        "the SDK forwards the captured framebuffer to the vision model",
      );
      await app.browser.switchToWindow(label(profile));
      await waitFor(async () => !(await state()).luaActive, "agent Back hold exits the actual Lua app", 30000);
      results = await agentTools(app, [{ id: "rerun-with-agent", name: "push_to_device", arguments: {} }]);
      assert.doesNotMatch(JSON.stringify(results), /No device is selected|设备离线|offline/);
      await app.browser.switchToWindow(label(profile));
      await waitFor(async () => (await state()).luaActive, "legacy agent run uses the selected simulator", 120000);
      await expectLuaScreen();
      await app.screenshot("simulator-agent-tools-native");

      // Exercise the ordinary edit/run loop in the same device window. Lua owns
      // its resources until the physical Back long-press exits the application.
      await press("back", 1400);
      await waitFor(async () => !(await state()).luaActive, "Back exits the current Lua app", 30000);
      await app.browser.switchToWindow("main");
      await app.browser.$('button[aria-label="运行"]').click();
      await app.browser.$(".run-target-row").waitForDisplayed();
      await app.browser.$(".run-target-row").click();
      await app.browser.switchToWindow(label(profile));
      await waitFor(async () => (await state()).luaActive, "project runs again in the same device", 120000);
      await expectLuaScreen();
      assert.equal(await app.browser.$(".preview-notes .error").isExisting(), false);
      await app.screenshot("simulator-repeated-project-run-native");

      // Normal close waits for firmware shutdown, including its pending preference writes.
      await app.browser.closeWindow();
      await waitFor(
        async () => !(await app.browser.getWindowHandles()).includes(label(profile)),
        "normal close flushes and destroys the WebView",
        15000,
      );
      await app.browser.switchToWindow("main");
      await app.restart();
      await openFromMain(profile);
      assert.equal(
        await app.browser.execute(() => location.origin),
        origin,
        "storage origin stays fixed across process restart",
      );
      await start(false);
      await waitFor(async () => (await state()).tutorialCompleted, "completed setup survives process restart", 30000);
      assert.equal((await state()).tutorialActive, false);
      assert.ok(
        (await apps()).some((app) => app.app_id === "alpha"),
        "installed Lua app survives process restart",
      );
      await waitFor(
        async () => {
          const response = await firmwareCommand(app, {
            method: "tools.invoke",
            tool: "lua.app.run",
            args: { app_id: "alpha" },
          });
          if (response.ok) return true;
          // Only explicit rejection or the SDK's pre-submission ABI check is safe
          // to retry. A deadline or other ambiguous write must fail this check.
          assert.ok(
            response.code === -18 ||
              (response.code === -1000 && response.rpcCode === 8) ||
              response.reason.startsWith("Error: Firmware GizClaw interface is not ready;"),
            JSON.stringify(response),
          );
          return false;
        },
        "stored app is admitted after startup",
        60000,
      );
      await waitFor(async () => (await state()).luaActive, "persisted app launches without reinstalling", 90000);
      await expectLuaScreen();
      await app.screenshot("simulator-restored-after-restart-native");

      await app.browser.switchToWindow("main");
      assert.equal(
        (await app.invoke("simulator_prepare_update")).ok,
        true,
        "updater preparation waits for the running Lua firmware to save and close",
      );
      assert.ok(!(await app.browser.getWindowHandles()).includes(label(profile)));
      assert.equal((await app.invoke("simulator_cancel_update")).ok, true);
      await openFromMain(profile);
      await start(false);
      assert.ok(
        (await apps()).some((app) => app.app_id === "alpha"),
        "updater preparation preserves installed apps",
      );

      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_profile_create", { profile: second })).ok, true);
      await openFromMain(second);
      await start(true);
      assert.equal(
        await app.browser.execute(() => location.origin),
        origin,
        "same origin is isolated by UUID data store",
      );
      await waitFor(async () => (await state()).tutorialActive, "another simulator has its own setup progress", 30000);
      assert.ok(!(await apps()).some((app) => app.app_id === "alpha"), "installed apps do not leak between simulators");
      assert.equal(
        (await app.invoke("simulator_profile_delete", { id: profile.id })).ok,
        false,
        "a simulator cannot delete another device",
      );
      await app.browser.execute(() => localStorage.setItem("gizmoth-test-deletion", "second-device"));
      await deleteCurrent(second);

      assert.equal((await app.invoke("simulator_profile_create", { profile: second })).ok, true);
      await openFromMain(second);
      assert.equal(
        await app.browser.execute(() => localStorage.getItem("gizmoth-test-deletion")),
        null,
        "deletion removes the UUID's browsing data",
      );
      await deleteCurrent(second);
      assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
      assert.deepEqual((await app.invoke("simulator_profiles_list")).value, []);
    }),
);
