import assert from "node:assert/strict";
import test from "node:test";
import { withApp } from "./native-app.mjs";

test(
  "native Permissions reads status without prompting and persists serial notification preference",
  { timeout: 90000 },
  (t) =>
    withApp(t, async (app) => {
      const status = await app.invoke("system_permissions");
      assert.equal(status.ok, true, JSON.stringify(status));
      const known = new Set(["granted", "denied", "prompt", "unknown", "unavailable"]);
      assert.deepEqual(Object.keys(status.value).sort(), ["bluetooth", "localNetwork", "notifications"]);
      for (const state of Object.values(status.value)) assert(known.has(state), `Unknown permission state: ${state}`);
      await app.browser.$('button[data-page="settings"]').click();
      await app.browser.$('[data-settings-section="grants"]').click();
      await app.browser.$(".grants-page").waitForDisplayed();
      const toggle = await app.browser.$('input[role="switch"][aria-label="串口任务通知"]');
      await toggle.waitForEnabled();
      assert.equal(await toggle.isSelected(), true);
      await toggle.click();
      await app.browser.waitUntil(async () => !(await toggle.isSelected()) && (await toggle.isEnabled()));
      const persisted = await app.invoke("notification_settings");
      assert.deepEqual(persisted, { ok: true, value: { serialTasks: false } });
      await app.browser.$('[data-settings-section="general"]').click();
      await app.browser.$('[data-settings-section="grants"]').click();
      await toggle.waitForEnabled();
      assert.equal(await toggle.isSelected(), false);
      assert.equal((await app.browser.$$(".grant-state")).length, 3);
      assert.equal(await app.browser.$('.grants-page [role="alert"]').isExisting(), false);
      await app.screenshot("grants-native");
      await app.restart();
      assert.deepEqual(await app.invoke("notification_settings"), { ok: true, value: { serialTasks: false } });
      assert.equal(app.model.requests.length, 0);
    }),
);
