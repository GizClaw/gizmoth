import assert from "node:assert/strict";
import test from "node:test";
import { chooseSelect, setAppLocale, withApp } from "./native-app.mjs";

test("native dropdowns use HTML listboxes, keyboard focus and modal-safe portals", { timeout: 90000 }, (t) =>
  withApp(t, async (app) => {
    await app.openProject("alpha");
    await app.idle();
    await setAppLocale(app, "zh-CN");
    await app.browser.$('button.nav-item[aria-label="文档"]').click();
    await chooseSelect(app, "文档栏目", "start");
    const section = await app.browser.$('button[role="combobox"][aria-label="文档栏目"]');
    await section.waitForDisplayed();
    assert.equal(await app.browser.$("select").isExisting(), false);
    // A synthetic click intentionally supplies no platform focus; the component must focus itself.
    await app.browser.execute(() => {
      document.activeElement?.blur();
      document.querySelector('button[role="combobox"][aria-label="文档栏目"]').click();
    });
    const menu = await app.browser.$("[data-select-menu]:popover-open");
    await menu.waitForDisplayed();
    assert.equal(await app.browser.execute(() => document.activeElement?.getAttribute("role")), "combobox");
    await app.screenshot("custom-dropdown-section-native");
    await app.browser.keys("ArrowDown");
    await app.browser.keys("Enter");
    assert.equal(await section.getAttribute("data-value"), "lua");
    assert.equal(await section.getAttribute("aria-expanded"), "false");
    await chooseSelect(app, "文档栏目", "start");
    await app.browser.$('button.nav-item[aria-label="Providers"]').click();
    await app.browser.$('//button[normalize-space()="添加 Provider"]').click();
    const openai = await app.browser.$('//dialog//button[contains(normalize-space(), "OpenAI")]');
    await openai.waitForDisplayed();
    await openai.click();
    const api = await app.browser.$('button[role="combobox"][aria-label="接口类型"]');
    await api.waitForDisplayed();
    await api.click();
    const apiMenu = await app.browser.$("[data-select-menu]:popover-open");
    await apiMenu.waitForDisplayed();
    assert.equal(
      await app.browser.execute(
        () => !!document.querySelector("[data-select-menu]:popover-open")?.closest("dialog[open]"),
      ),
      true,
    );
    await app.browser.keys("Escape");
    assert.equal(await api.getAttribute("aria-expanded"), "false");
    assert.equal(
      await app.browser.$("dialog[open]").isDisplayed(),
      true,
      "Escape closes the dropdown before its dialog",
    );
    await chooseSelect(app, "接口类型", "anthropic-messages");
    assert.equal(await api.getAttribute("data-value"), "anthropic-messages");
    assert.equal(await app.browser.$("select").isExisting(), false);
  }),
);
