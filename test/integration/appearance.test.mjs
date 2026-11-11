import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { chooseSelect, waitFor, withApp } from "./native-app.mjs";

test("native appearance and language persist, update docs and preserve other settings", { timeout: 90000 }, (t) =>
  withApp(t, async (app) => {
    await app.browser.$('button[data-page="settings"]').click();
    await chooseSelect(app, "主题", "dark");
    await chooseSelect(app, "语言", "en");
    await app.browser.$('button[role="combobox"][aria-label="Theme"]').waitForDisplayed();
    assert.equal(await app.browser.execute(() => document.documentElement.dataset.theme), "dark");
    assert.equal(await app.browser.execute(() => document.documentElement.lang), "en");
    const cardColors = await app.browser.execute(() => {
      const surface = getComputedStyle(document.documentElement).getPropertyValue("--mds-surface").trim();
      const probe = document.createElement("span");
      probe.style.backgroundColor = surface;
      document.body.append(probe);
      const expected = getComputedStyle(probe).backgroundColor;
      probe.remove();
      return { surface, expected, actual: getComputedStyle(document.querySelector(".card")).backgroundColor };
    });
    assert.ok(cardColors.surface, "the design system supplies the active surface token");
    assert.equal(cardColors.actual, cardColors.expected, "cards follow the active design-system surface");
    await app.screenshot("settings-polish-native-dark-en");
    await app.browser.$('button[data-page="docs"]').click();
    await app.browser.$('.documentation-layout[lang="en"]').waitForDisplayed();
    assert.match(await app.browser.$(".docs-markdown").getText(), /portable firmware/);
    assert.equal(await app.browser.$('[aria-label="文档语言 / Documentation language"]').isExisting(), false);
    await app.screenshot("appearance-dark-english-native");
    await app.browser.$('button[data-page="settings"]').click();
    const windowSwitch = await app.browser.$('input[role="switch"]');
    await windowSwitch.waitForEnabled();
    await windowSwitch.click();
    await waitFor(() => windowSwitch.isSelected(), "the window setting is saved through its switch");
    let saved = JSON.parse(await readFile(join(app.data, "desktop.json"), "utf8"));
    assert.deepEqual([saved.theme, saved.language, saved.keepInTray], ["dark", "en", true]);
    await app.reload();
    await app.browser.$(".project-list").waitForDisplayed();
    assert.deepEqual(
      await app.browser.execute(() => [document.documentElement.dataset.theme, document.documentElement.lang]),
      ["dark", "en"],
    );
    assert.equal((await app.invoke("set_theme_preference", { preference: "light" })).ok, true);
    assert.equal((await app.invoke("set_language_preference", { preference: "zh-CN" })).ok, true);
    await waitFor(
      () =>
        app.browser.execute(
          () => document.documentElement.dataset.theme === "light" && document.documentElement.lang === "zh-CN",
        ),
      "native app events update all preferences",
    );
    await app.browser.$('button[data-page="docs"]').click();
    await app.browser.$('.documentation-layout[lang="zh-CN"]').waitForDisplayed();
    assert.match(await app.browser.$(".docs-markdown").getText(), /可移植平台/);
    assert.equal((await app.invoke("set_theme_preference", { preference: "auto" })).ok, false);
    assert.equal((await app.invoke("set_language_preference", { preference: "fr" })).ok, false);
    assert.equal((await app.invoke("set_theme_preference", { preference: "system" })).ok, true);
    assert.equal((await app.invoke("set_language_preference", { preference: "system" })).ok, true);
    saved = JSON.parse(await readFile(join(app.data, "desktop.json"), "utf8"));
    assert.deepEqual([saved.theme, saved.language, saved.keepInTray], ["system", "system", true]);
    assert.equal(app.model.requests.length, 0);
  }),
);
