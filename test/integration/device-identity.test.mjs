import assert from "node:assert/strict";
import test from "node:test";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { waitFor, withApp } from "./native-app.mjs";

test("native device names persist and information rows keep a single height", { timeout: 90000 }, (t) =>
  withApp(t, async (app) => {
    const device = { id: "a".repeat(64), name: "测试设备", baseUrl: "https://device.example", icon: "h106-tiga" };
    await writeFile(join(app.data, "devices.json"), JSON.stringify([device]));
    await app.browser.$('button[data-page="devices"]').click();
    await app.browser.$('button[aria-label="刷新设备与项目"]').click();
    await app.browser.$(".device-list-row").waitForDisplayed();
    await app.browser.$(".device-list-row").click();
    await app.browser.$(".device-name-edit").waitForEnabled();
    await app.browser.$(".device-name-edit").click();
    const name = await app.browser.$(".device-name-form input");
    await name.waitForDisplayed();
    assert.equal(await name.getValue(), device.name);
    const centers = await app.browser.execute(() =>
      [...document.querySelectorAll(".device-name-form input, .device-name-form button, .device-remove")].map(
        (element) => {
          const bounds = element.getBoundingClientRect();
          return bounds.top + bounds.height / 2;
        },
      ),
    );
    assert.equal(centers.length, 4);
    assert.ok(Math.max(...centers) - Math.min(...centers) <= 0.5, "name edit controls share one vertical center");
    await app.screenshot("device-name-edit-alignment-native");
    await name.setValue("工作台设备");
    await app.browser.$('button[aria-label="保存设备名称"]').click();
    await waitFor(
      async () => (await app.browser.$(".device-name-editor h1").getText()) === "工作台设备",
      "saved device name",
    );
    assert.match(await app.browser.$(".titlebar-page").getText(), /工作台设备/);
    assert.deepEqual(JSON.parse(await readFile(join(app.data, "devices.json"), "utf8")), [
      { ...device, name: "工作台设备" },
    ]);
    assert.deepEqual((await app.invoke("list_devices")).value, [{ ...device, name: "工作台设备" }]);

    await app.browser.$(".device-name-edit").click();
    // WebView2's WebDriver clear does not emit React's input event reliably.
    // Replace through real editing keys so the controlled field cannot restore the old value.
    await name.click();
    await app.browser.keys([process.platform === "darwin" ? "Meta" : "Control", "a"]);
    await app.browser.keys("Backspace");
    await name.addValue("   ");
    await waitFor(async () => (await name.getValue()) === "   ", "whitespace-only name input");
    await app.browser.$('button[aria-label="保存设备名称"]').waitForEnabled({ reverse: true });
    assert.equal(await app.browser.$('button[aria-label="保存设备名称"]').isEnabled(), false);
    await name.addValue("不保存的名称");
    await app.browser.keys("Escape");
    assert.equal(await app.browser.$(".device-name-editor h1").getText(), "工作台设备");
    await app.browser.$(".device-info-toggle").click();
    const heights = await app.browser.execute(() =>
      [...document.querySelectorAll(".device-identity-strip, .device-identity-details > div")].map(
        (row) => row.getBoundingClientRect().height,
      ),
    );
    assert.deepEqual(heights, [44, 44, 44, 44]);
    // The isolated fixture denies key reads. Its error notice must be dismissible,
    // and focusing the read-only field must preserve access to Retry afterward.
    await app.browser.$(".device-key-notice").waitForDisplayed();
    await app.browser.$(".device-key-notice button:last-child").click();
    assert.equal(await app.browser.$(".device-key-notice").isExisting(), false);
    const keyInput = await app.browser.$(".device-key-editor input");
    await keyInput.click();
    await app.browser.$(".device-key-notice").waitForDisplayed();
    await app.browser.keys("Escape");
    assert.equal(await app.browser.$(".device-key-notice").isExisting(), false);
    await app.screenshot("device-identity-single-line-native");

    await app.browser.refresh();
    await app.browser.$('button[data-page="devices"]').waitForDisplayed();
    await app.browser.$('button[data-page="devices"]').click();
    await app.browser.$(".device-row-name").waitForDisplayed();
    assert.equal(await app.browser.$(".device-row-name").getText(), "工作台设备");
    assert.equal(app.model.requests.length, 0);
  }),
);
