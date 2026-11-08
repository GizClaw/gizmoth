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
      const started = await agentTools(app, [{ id: "auto-start", name: "run_in_simulator", arguments: {} }]);
      const status = JSON.parse(started["auto-start"]);
      assert.equal(status.error, null);
      const profiles = (await app.invoke("simulator_profiles_list")).value;
      assert.equal(profiles.length, 1);
      const label = `simulator-${profiles[0].id.replaceAll("-", "")}`;
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
      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_profile_delete", { id: profiles[0].id })).ok, true);
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
      const start = async (installFirmware) => {
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
        await waitFor(
          async () => (await app.browser.$(".preview-state").getText()).includes("等待启动"),
          "firmware becomes ready",
          90000,
        );
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
        // Optional test capability boundary for the OS WebKit encoded-audio worker crash.
        // This leaves radios, real firmware commands, storage and Lua execution intact.
        if (process.env.GIZMOTH_DISABLE_RTC_TRANSFORM_TEST === "1")
          await app.browser.execute(() => {
            const child = document.querySelector("iframe").contentWindow;
            Object.defineProperty(child, "RTCRtpScriptTransform", { configurable: true, value: undefined });
          });
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
        await press("power");
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
      await start(true);
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
