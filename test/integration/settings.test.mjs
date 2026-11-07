import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { withApp } from "./native-app.mjs";

test("native Settings exposes the selected open-source projects and handles opener errors", { timeout: 90000 }, (t) =>
  withApp(t, async (app) => {
    await app.browser.$('button[data-page="settings"]').click();
    await app.browser.$(".settings-navigation").waitForDisplayed();
    assert.equal(await app.browser.$('[data-settings-section="general"]').getAttribute("aria-current"), "page");
    await app.browser.$('[data-settings-section="open-source"]').click();
    await app.browser.$(".open-source-project").waitForDisplayed();
    assert.equal((await app.browser.$$(".open-source-project")).length, 3);
    const expected = ["https://github.com/GizClaw/gizos", "https://github.com/GizClaw/gizclaw", "https://github.com/GizClaw/gizmoth"];
    for (const selector of [".open-source-project-link", ".open-source-star"]) {
      const urls = await app.browser.execute(selector => [...document.querySelectorAll(selector)].map(link => link.getAttribute("href")), selector);
      assert.deepEqual(urls, expected);
    }
    assert.equal(await app.browser.$('[data-settings-section="open-source"]').getAttribute("aria-current"), "page");
    assert.equal(await app.browser.$(".settings-star-link").isExisting(), false);
    // The isolated fixture deliberately grants no OS opener permission. A real
    // invocation must surface its denial in the App, without launching a browser.
    await (await app.browser.$$(".open-source-star"))[2].click();
    await app.browser.$(".open-source-page > .error.banner").waitForDisplayed();
    assert.match(await app.browser.$(".open-source-page > .error.banner").getText(), /opener\.open_url not allowed/);
    assert.equal((await app.browser.$$(".open-source-project")).length, 3, "an opener error keeps the App page intact");
    assert.equal(app.model.requests.length, 0);
  }),
);

test("native About owns the version and updater while project creation stays in content", { timeout: 90000 }, (t) =>
  withApp(t, async (app) => {
    const { version } = JSON.parse(await readFile(new URL("../../package.json", import.meta.url), "utf8"));
    await app.browser.$('button[data-page="settings"]').click();
    await app.browser.$('[data-settings-section="about"]').click();
    await app.browser.$(".about-page").waitForDisplayed();
    assert.equal(await app.browser.$(".about-version").getText(), `v${version}`);
    assert.equal((await app.browser.$$(".about-update-actions button")).length, 1);
    assert.equal(await app.browser.$(".titlebar-utilities .app-version").isExisting(), false);
    assert.equal(await app.browser.$(".sidebar .connect-device").isExisting(), false);
    await app.screenshot("about-settings-native");
    await app.browser.$('button[data-page="projects"]').click();
    await app.browser.$(".projects-page .text-action").waitForDisplayed();
    assert.equal((await app.browser.$$(".titlebar-page .header-actions button")).length, 0);
    await app.browser.$(".projects-page .text-action").click();
    await app.browser.$("dialog").waitForDisplayed();
    assert.equal(await app.browser.$("dialog h2").getText(), "新建项目");
    await app.browser.$('dialog .dialog-actions button:first-child').click();
    assert.equal(app.model.requests.length, 0);
  }),
);
