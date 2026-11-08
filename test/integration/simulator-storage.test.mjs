import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import test from "node:test";
import { waitFor, withApp } from "./native-app.mjs";

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
                await app.browser.executeAsync((done) => {
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
        app.browser.executeAsync((value, done) => {
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
              done({ preference: localStorage.getItem(key), filesystem: read.result ?? null, origin: location.origin });
            };
            transaction.onerror = () => {
              database.close();
              done({ failure: String(transaction.error) });
            };
          };
        }, value);
      const remove = async (device) => {
        const button = await app.browser.$(".simulator-titlebar .simulator-delete-text");
        await button.waitForEnabled();
        await button.click();
        await app.browser.$(".dialog-actions button.danger").click();
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
      const opened = await app.browser.executeAsync(
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
      await remove(second);
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
      assert.equal((await app.invoke("simulator_profile_delete", { id: first.id })).ok, true);
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
