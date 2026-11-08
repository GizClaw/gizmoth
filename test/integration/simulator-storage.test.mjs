import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import test from "node:test";
import { runNativeAsync, waitFor, withApp } from "./native-app.mjs";

// Test the actual native storage boundary without starting network/media firmware.
test(
  "native simulator storage persists across process restart and stays private to its device",
  { skip: process.env.GIZMOTH_LIVE_SIMULATOR_TEST !== "1", timeout: 180000 },
  (t) =>
    withApp(t, async (app) => {
      const first = { id: randomUUID(), name: "Storage A", model: "h106-tiga", firmwareId: "", view: "screen" };
      const second = { ...first, id: randomUUID(), name: "Storage B" };
      const label = (device) => `simulator-${device.id.replaceAll("-", "")}`;
      const open = async (device) => {
        await app.browser.switchToWindow("main");
        await app.browser.$('button[data-page="projects"]').click();
        await app.browser.$('button[data-page="simulators"]').click();
        const button = await app.browser.$(`button[aria-label="打开 ${device.name}"]`);
        await button.waitForDisplayed();
        await button.click();
        await waitFor(
          async () => (await app.browser.getWindowHandles()).includes(label(device)),
          "device window opens",
        );
        await app.browser.switchToWindow(label(device));
        await app.browser
          .$(".game-preview")
          .waitForDisplayed({ timeout: 30000 })
          .catch(async (error) => {
            console.error(
              JSON.stringify(
                await runNativeAsync(app.browser, (done) => {
                  const read = window.__TAURI__?.core?.invoke("theme_preference").then(
                    (value) => ({ value }),
                    (error) => ({ reason: String(error) }),
                  );
                  Promise.race([
                    read,
                    new Promise((resolve) => setTimeout(() => resolve({ pending: true }), 1000)),
                  ]).then((preference) =>
                    done({
                      url: location.href,
                      tauri: !!window.__TAURI_INTERNALS__,
                      preference,
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
      };
      const storage = async (value = null) =>
        runNativeAsync(
          app.browser,
          (value, done) => {
            const key = "gizmoth-storage-test";
            if (value !== null) localStorage.setItem(key, value);
            const request = indexedDB.open(key, 1);
            request.onupgradeneeded = () => request.result.createObjectStore("values");
            request.onerror = () => done({ failure: String(request.error) });
            request.onsuccess = () => {
              const database = request.result;
              const transaction = database.transaction("values", value === null ? "readonly" : "readwrite");
              const store = transaction.objectStore("values");
              if (value !== null) store.put(value, "device");
              const read = store.get("device");
              transaction.oncomplete = () => {
                database.close();
                done({
                  preference: localStorage.getItem(key),
                  filesystem: read.result ?? null,
                  origin: location.origin,
                });
              };
              transaction.onerror = () => {
                database.close();
                done({ failure: String(transaction.error) });
              };
            };
          },
          value,
        );
      const remove = async (device, overlapClose = false) => {
        if (overlapClose) {
          await waitFor(
            () =>
              app.browser.execute(() => {
                const host = document.querySelector(".game-preview");
                let fiber = host?.[Object.keys(host).find((key) => key.startsWith("__reactFiber"))];
                let player;
                for (; fiber && !player; fiber = fiber.return) {
                  for (let hook = fiber.memoizedState; hook; hook = hook.next)
                    if (hook.memoizedState?.current?.executeCommand) player = hook.memoizedState.current;
                }
                if (!player) return false;
                const original = player.shutdown.bind(player);
                let calls = 0;
                player.shutdown = () =>
                  new Promise((resolve, reject) => {
                    // Force native Close to overlap the pending deletion save. An
                    // independent second shutdown would overtake the first one.
                    setTimeout(
                      () => {
                        try {
                          Promise.resolve(original()).then(resolve, reject);
                        } catch (error) {
                          reject(error);
                        }
                      },
                      ++calls === 1 ? 700 : 1,
                    );
                  });
                return true;
              }),
            "controller is ready for teardown overlap",
          );
        }
        const button = await app.browser.$(".simulator-titlebar .simulator-delete-text");
        await button.waitForEnabled();
        await button.click();
        await app.browser.$(".dialog-actions button.danger").click();
        if (overlapClose) await app.browser.closeWindow();
        await waitFor(
          async () => !(await app.browser.getWindowHandles()).includes(label(device)),
          "device window is destroyed",
        );
        await app.browser.switchToWindow("main");
        await waitFor(
          async () => !(await app.invoke("simulator_profiles_list")).value.some((entry) => entry.id === device.id),
          "device data store removal completes",
          45000,
        ).catch(async (error) => {
          console.error(
            "Deletion retry from main:",
            JSON.stringify(await app.invoke("simulator_profile_delete", { id: device.id })),
          );
          throw error;
        });
      };

      assert.equal((await app.invoke("simulator_profile_create", { profile: first })).ok, true);
      assert.equal(
        (await app.invoke("simulator_profile_update", { id: first.id, patch: { model: "h106-zero" } })).ok,
        false,
      );
      // Registry reads on the UI thread must not deadlock an asynchronous window
      // builder that needs that same thread. Exercise both operations together.
      const opened = await runNativeAsync(
        app.browser,
        (id, token, done) => {
          const invoke = window.__TAURI__.core.invoke;
          Promise.all([
            invoke("open_simulator", { profileId: id, token }),
            ...Array.from({ length: 8 }, () => invoke("simulator_profiles_list")),
          ]).then(
            ([label]) => done({ ok: true, label }),
            (error) => done({ ok: false, reason: String(error) }),
          );
        },
        first.id,
        randomUUID(),
      );
      assert.equal(opened.ok, true);
      assert.equal(opened.label, label(first));
      await app.browser.switchToWindow(label(first));
      await app.browser.$(".game-preview").waitForDisplayed({ timeout: 30000 });
      const saved = await storage("first-device");
      assert.equal(saved.preference, "first-device");
      assert.equal(saved.filesystem, "first-device");
      await app.screenshot("simulator-fixed-device-native");
      await app.browser.closeWindow();
      await waitFor(
        async () => !(await app.browser.getWindowHandles()).includes(label(first)),
        "normal Close finishes its save barrier",
      );
      await app.browser.switchToWindow("main");
      await app.restart();
      await open(first);
      assert.deepEqual(
        await storage(),
        saved,
        "both preference and filesystem storage survive restarting the whole app process",
      );
      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_profile_create", { profile: second })).ok, true);
      await open(second);
      assert.deepEqual(
        await storage(),
        { preference: null, filesystem: null, origin: saved.origin },
        "same-model devices do not share data, even on the same origin",
      );
      await storage("second-device");
      for (const target of ["main", label(first)]) {
        assert.equal(
          (await app.invoke("plugin:window|destroy", { label: target })).ok,
          false,
          "simulator code cannot use core destruction to target another window",
        );
        assert.ok((await app.browser.getWindowHandles()).includes(target));
      }
      assert.equal(
        (await app.invoke("simulator_profile_delete", { id: first.id })).ok,
        false,
        "device windows cannot delete another device",
      );
      await remove(second, true);
      assert.equal((await app.invoke("simulator_profile_create", { profile: second })).ok, true);
      await open(second);
      assert.deepEqual(
        await storage(),
        { preference: null, filesystem: null, origin: saved.origin },
        "deletion clears the entire device's persistent store",
      );
      await remove(second);
      await app.browser.switchToWindow(label(first));
      assert.deepEqual(await storage(), saved, "deleting the other device preserves this one's data");
      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_window_destroy", { label: label(first) })).ok, false);
      assert.ok((await app.browser.getWindowHandles()).includes(label(first)));
      await app.browser.switchToWindow(label(first));
      const closing = app.browser.closeWindow().then(() => null, (error) => String(error));
      await app.browser.switchToWindow("main");
      const deleted = await app.invoke("simulator_profile_delete", { id: first.id });
      assert.equal(await closing, null);
      // Close may win before Windows can arm a watch for the old controller.
      // A rejected attempt must release its reservation so deletion can retry.
      if (!deleted.ok) assert.equal((await app.invoke("simulator_profile_delete", { id: first.id })).ok, true);
      assert.ok(!(await app.browser.getWindowHandles()).includes(label(first)));
      assert.deepEqual((await app.invoke("simulator_profiles_list")).value, []);
      assert.equal((await app.invoke("simulator_profile_create", { profile: first })).ok, true);
      await open(first);
      assert.deepEqual(
        await storage(),
        { preference: null, filesystem: null, origin: saved.origin },
        "manager deletion waits for destruction and clears the device's store",
      );
      const updateSaved = await storage("saved-before-update");
      assert.equal((await app.invoke("simulator_prepare_update")).ok, false, "a device cannot close all simulators");
      await app.browser.switchToWindow("main");
      assert.equal((await app.invoke("simulator_prepare_update")).ok, true);
      assert.ok(!(await app.browser.getWindowHandles()).includes(label(first)));
      assert.equal(
        (await app.invoke("open_simulator", { profileId: first.id, token: randomUUID() })).ok,
        false,
        "new simulator opens are blocked between preparation and restart",
      );
      assert.equal((await app.invoke("simulator_cancel_update")).ok, true);
      await open(first);
      assert.deepEqual(await storage(), updateSaved, "update preparation saves without deleting the device");
      await remove(first);
      assert.deepEqual((await app.invoke("simulator_profiles_list")).value, []);
    }),
);

test(
  "failed initial Windows tracking can recover without reopening the simulator",
  {
    skip: process.platform !== "win32" || process.env.GIZMOTH_LIVE_SIMULATOR_TEST !== "1",
    timeout: 120000,
  },
  (t) => withApp(t, async (app) => {
    for (const mode of ["update", "update-during-open", "delete"]) {
      const profile = { id: randomUUID(), name: `Failed open ${mode}`, model: "h106-tiga", firmwareId: "", view: "screen" };
      assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
      let opened;
      if (mode === "update-during-open") {
        await app.browser.execute((id, token) => {
          window.__trackingOpen = null;
          window.__TAURI__.core.invoke("open_simulator", { profileId: id, token }).then(
            (value) => { window.__trackingOpen = { ok: true, value }; },
            (error) => { window.__trackingOpen = { ok: false, reason: String(error) }; },
          );
        }, profile.id, randomUUID());
        await waitFor(
          async () => (await app.browser.getWindowHandles()).includes(`simulator-${profile.id.replaceAll("-", "")}`),
          "opening window exists before tracking failure returns",
        );
        assert.equal((await app.invoke("simulator_prepare_update")).ok, true);
        await waitFor(() => app.browser.execute(() => window.__trackingOpen !== null), "failed opening completes during update preparation");
        opened = await app.browser.execute(() => window.__trackingOpen);
        assert.equal((await app.invoke("simulator_cancel_update")).ok, true);
      } else {
        opened = await app.invoke("open_simulator", { profileId: profile.id, token: randomUUID() });
      }
      assert.equal(opened.ok, false);
      assert.ok(opened.reason.includes("Injected initial simulator tracking failure"));
      await waitFor(
        async () => !(await app.browser.getWindowHandles()).includes(`simulator-${profile.id.replaceAll("-", "")}`),
        "failed-open window is destroyed",
      );
      // The injected failure occurs before PID lookup, handle capture and event
      // subscription. Recover through the real retained environment after close,
      // without reopening the profile or restarting the application process.
      if (mode === "update") {
        assert.equal((await app.invoke("simulator_prepare_update")).ok, true);
        assert.equal((await app.invoke("simulator_cancel_update")).ok, true);
      }
      assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
      assert.deepEqual((await app.invoke("simulator_profiles_list")).value, []);
    }
  }, { failSimulatorTracking: true }),
);

test(
  "update preparation closes a simulator whose opening finishes during shutdown",
  { skip: process.env.GIZMOTH_LIVE_SIMULATOR_TEST !== "1", timeout: 90000 },
  (t) => withApp(t, async (app) => {
    const profile = { id: randomUUID(), name: "Opening during update", model: "h106-tiga", firmwareId: "", view: "screen" };
    const label = `simulator-${profile.id.replaceAll("-", "")}`;
    assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
    await app.browser.execute((id, token) => {
      window.__heldOpening = null;
      window.__TAURI__.core.invoke("open_simulator", { profileId: id, token }).then(
        (value) => { window.__heldOpening = { ok: true, value }; },
        (error) => { window.__heldOpening = { ok: false, reason: String(error) }; },
      );
    }, profile.id, randomUUID());
    await waitFor(async () => (await app.browser.getWindowHandles()).includes(label), "opening native window is registered");
    assert.equal(await app.browser.execute(() => window.__heldOpening), null, "opening is still reserved");
    // Preparation releases the fixture's opening gate. It must observe the new
    // live window after the reservation drains, then close it before succeeding.
    assert.equal((await app.invoke("simulator_prepare_update")).ok, true);
    await app.browser.switchToWindow("main");
    await waitFor(() => app.browser.execute(() => window.__heldOpening !== null), "opening finishes before update readiness");
    assert.deepEqual(await app.browser.execute(() => window.__heldOpening), { ok: true, value: label });
    assert.ok(!(await app.browser.getWindowHandles()).includes(label), "update readiness requires the new window to be gone");
    assert.equal((await app.invoke("simulator_cancel_update")).ok, true);
    assert.equal((await app.invoke("simulator_profile_delete", { id: profile.id })).ok, true);
  }, { holdSimulatorOpening: true }),
);
