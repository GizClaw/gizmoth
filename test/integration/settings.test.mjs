import assert from "node:assert/strict";
import test from "node:test";
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
    assert.equal(await app.browser.$(".settings-star-link").getAttribute("href"), "https://github.com/GizClaw/gizmoth");
    // The isolated fixture deliberately grants no OS opener permission. A real
    // invocation must surface its denial in the App, without launching a browser.
    await app.browser.$(".settings-star-link").click();
    await app.browser.$(".main > .error.banner").waitForDisplayed();
    assert.match(await app.browser.$(".main > .error.banner").getText(), /opener\.open_url not allowed/);
    assert.equal((await app.browser.$$(".open-source-project")).length, 3, "an opener error keeps the App page intact");
    assert.equal(app.model.requests.length, 0);
  }),
);
