import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import test from "node:test";
import { remote } from "webdriverio";

// The real Compiling page in the browser demo, with an in-memory artifact fixture (artifact-list.mjs).
// This test owns its Vite server on a separate port so it can run beside the popup test.
const port = 1423;
const origin = `http://127.0.0.1:${port}`;
const shots = process.env.GIZMOTH_ARTIFACT_SHOTS;

async function startServer() {
  const server = spawn(
    process.execPath,
    ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  let log = "";
  const collect = (chunk) => {
    log = (log + chunk.toString()).slice(-12000);
  };
  server.stdout.on("data", collect);
  server.stderr.on("data", collect);
  const deadline = Date.now() + 45000;
  while (true) {
    if (server.exitCode !== null || server.signalCode !== null)
      throw new Error(`Vite exited ${server.exitCode ?? server.signalCode}: ${log}`);
    try {
      const response = await fetch(`${origin}/test/browser/artifact-list.html`, { signal: AbortSignal.timeout(1000) });
      if (response.ok) return { server, log: () => log };
    } catch {
      /* Wait until this test's server accepts requests. */
    }
    if (Date.now() >= deadline) {
      server.kill();
      throw new Error(`Vite did not become ready: ${log}`);
    }
    await pause(250);
  }
}

/** Geometry of every artifact row, measured in the page. */
const measure = (browser) =>
  browser.execute(() => {
    const box = (element) => {
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom, width: rect.width };
    };
    const list = document.querySelector(".compile-artifact-list");
    return {
      list: box(list),
      overflow: list.scrollWidth - list.clientWidth,
      rows: [...document.querySelectorAll(".compile-artifact-item")].map((item) => {
        const title = item.querySelector(".compile-artifact-title");
        const meta = item.querySelector(".compile-artifact-meta-text");
        return {
          id: item.id,
          pinned: item.dataset.pinned === "true",
          installable: item.dataset.installable === "true",
          title: box(title),
          truncated: title.scrollWidth > title.clientWidth,
          meta: getComputedStyle(meta).display === "none" ? null : box(meta),
          time: box(item.querySelector(".compile-artifact-time")),
          trigger: box(item.querySelector('.compile-artifact-actions button[aria-haspopup="menu"]')),
        };
      }),
    };
  });

function assertAligned(layout, { meta }) {
  const [first, ...rest] = layout.rows;
  assert.ok(layout.rows.length >= 2);
  assert.ok(layout.overflow <= 0, "the list must not scroll horizontally");
  for (const row of rest) {
    assert.ok(Math.abs(row.time.right - first.time.right) < 0.5, `${row.id} time right edge`);
    assert.ok(Math.abs(row.trigger.left - first.trigger.left) < 0.5, `${row.id} menu trigger`);
    if (meta) assert.ok(Math.abs(row.meta.left - first.meta.left) < 0.5, `${row.id} file summary column`);
    else assert.equal(row.meta, null, `${row.id} hides the file summary at narrow widths`);
  }
  for (const row of layout.rows) {
    assert.ok(row.title.right <= (row.meta ?? row.time).left + 0.5, `${row.id} title stays in its column`);
    assert.ok(row.trigger.right <= layout.list.right + 0.5, `${row.id} menu stays inside the list`);
  }
  assert.ok(layout.rows.some((row) => row.pinned) && layout.rows.some((row) => !row.pinned));
  assert.ok(layout.rows.some((row) => row.installable) && layout.rows.some((row) => !row.installable));
}

const item = (browser, id) => browser.$(`#artifact-${id}`);
const row = (browser, id) => browser.$(`#artifact-${id} .compile-artifact-row`);
const trigger = (browser, id) => browser.$(`#artifact-${id} .compile-artifact-actions button[aria-haspopup="menu"]`);
const focused = (browser, text) =>
  browser.waitUntil(async () => (await browser.execute(() => document.activeElement?.textContent.trim())) === text, {
    timeout: 5000,
    timeoutMsg: `focus did not reach ${text}`,
  });
async function openMenu(browser, id) {
  const button = await trigger(browser, id);
  // The trigger reflects the popover's asynchronous toggle event; wait until a previous menu settled.
  await browser.waitUntil(async () => (await button.getAttribute("aria-expanded")) === "false", { timeout: 5000 });
  await button.click();
  const menu = await browser.$(`#${CSS_ESCAPE(await button.getAttribute("aria-controls"))}`);
  await browser.waitUntil(() => browser.execute((element) => element.matches(":popover-open"), menu), {
    timeout: 5000,
    timeoutMsg: `menu for ${id} did not open`,
  });
  return menu;
}
const CSS_ESCAPE = (value) => value.replace(/([^\w-])/g, "\\$1");
const menuItems = (browser, menu) =>
  browser.execute(
    (element) =>
      [...element.querySelectorAll('[role="menuitem"]')].map((button) => ({
        text: button.textContent.trim(),
        disabled: button.disabled,
        title: button.title,
      })),
    menu,
  );
async function openArtifacts(browser, group) {
  // The Compiling sidebar group may start collapsed; expand it before choosing Artifacts.
  const toggle = await browser.$(".sidebar-toggle");
  await toggle.waitForExist({ timeout: 20000 });
  if ((await toggle.getAttribute("aria-expanded")) === "false") await toggle.click();
  const heading = await browser.$(`button.nav-heading[aria-label="${group}"]`);
  await heading.waitForClickable({ timeout: 10000 });
  if ((await heading.getAttribute("aria-expanded")) !== "true") await heading.click();
  const link = await browser.$('button[data-page="compile-artifacts"]');
  await link.waitForClickable({ timeout: 10000 });
  await link.click();
}
async function hideSidebarOverlay(browser) {
  // At narrow widths the sidebar overlays the page; collapse it to see the whole list.
  const toggle = await browser.$(".sidebar-toggle");
  if ((await toggle.getAttribute("aria-expanded")) === "true") await toggle.click();
  await browser.waitUntil(async () => (await toggle.getAttribute("aria-expanded")) === "false", { timeout: 5000 });
  await pause(300);
}
const calls = (browser) => browser.execute(() => window.__artifactFixture.calls);
async function screenshot(browser, name) {
  if (!shots) return;
  mkdirSync(shots, { recursive: true });
  await browser.saveScreenshot(join(shots, `${name}.png`));
}

test("artifact rows align, expand inline and act on one artifact from their menu", { timeout: 180000 }, async () => {
  const { server, log } = await startServer();
  let browser;
  try {
    browser = await remote({
      logLevel: "error",
      capabilities: {
        browserName: "chrome",
        "goog:chromeOptions": { args: ["--headless=new", "--no-sandbox", "--disable-dev-shm-usage", "--lang=zh-CN"] },
      },
    });
    await browser.setWindowSize(1180, 820);
    await browser.url(`${origin}/test/browser/artifact-list.html?demo=1`);
    await openArtifacts(browser, "编译");
    await browser.waitUntil(async () => (await browser.$$(".compile-artifact-item")).length === 8, {
      timeout: 20000,
      timeoutMsg: "fixture artifacts did not render",
    });
    await browser.execute(() => document.fonts.ready);

    // Collapsed rows share columns despite long titles, pins and missing install packages.
    const wide = await measure(browser);
    await screenshot(browser, "01-collapsed-wide");
    assertAligned(wide, { meta: true });
    assert.equal(wide.rows[0].truncated, true, "the long title truncates in its column");
    assert.equal((await browser.$$("dialog[open], .dialog")).length, 0);

    // Clicking a row toggles inline details; no dialog opens.
    const docs = await row(browser, "b-docs");
    assert.equal(await docs.getAttribute("aria-expanded"), "false");
    await docs.click();
    assert.equal(await docs.getAttribute("aria-expanded"), "true");
    const details = await browser.$(`#${CSS_ESCAPE(await docs.getAttribute("aria-controls"))}`);
    await details.waitForDisplayed();
    const text = await details.getText();
    assert.match(text, /b-docs/);
    assert.match(text, /out\/protocol\.pdf/);
    assert.match(text, /\/fixture\/workspace/);
    assert.equal(await details.$$('button[aria-label^="复制"]').length, 3, "two file paths and the ID copy");
    assert.equal((await browser.$$("dialog[open], .dialog")).length, 0);
    await details.$("button=在访达中显示").click();
    assert.deepEqual(await calls(browser), [["reveal", "/fixture/artifacts/b-docs/files"]]);
    assertAligned(await measure(browser), { meta: true });
    await screenshot(browser, "02-expanded-details");
    await docs.click();
    assert.equal(await docs.getAttribute("aria-expanded"), "false");
    assert.equal(await details.isDisplayed(), false);

    // Keyboard: the row button toggles with Enter/Space; the menu trigger is a separate stop.
    await browser.execute((element) => element.focus(), docs);
    await browser.keys("Enter");
    assert.equal(await docs.getAttribute("aria-expanded"), "true");
    await browser.keys(" ");
    assert.equal(await docs.getAttribute("aria-expanded"), "false");
    await browser.keys("Tab");
    assert.equal(
      await browser.execute(() => document.activeElement?.closest(".compile-artifact-actions")?.parentElement?.id),
      "artifact-b-docs",
    );
    await browser.keys("Enter");
    const keyboardMenu = await browser.$(
      `#${CSS_ESCAPE(await trigger(browser, "b-docs").getAttribute("aria-controls"))}`,
    );
    await browser.waitUntil(() => browser.execute((element) => element.matches(":popover-open"), keyboardMenu));
    // Install is visibly disabled for this non-firmware artifact, so focus starts on Pin.
    assert.deepEqual(await menuItems(browser, keyboardMenu), [
      { text: "安装", disabled: true, title: "这个产物没有可安装的固件包（.update.tar.zlib）。" },
      { text: "固定", disabled: false, title: "" },
      { text: "删除", disabled: false, title: "" },
    ]);
    await focused(browser, "固定");
    await browser.keys("ArrowDown");
    await focused(browser, "删除");
    await browser.keys("Escape");
    assert.equal(await browser.execute((element) => element.matches(":popover-open"), keyboardMenu), false);
    await browser.waitUntil(
      async () => (await browser.execute(() => document.activeElement?.getAttribute("aria-haspopup"))) === "menu",
      { timeout: 5000, timeoutMsg: "Escape did not return focus to the menu trigger" },
    );
    assert.equal(await docs.getAttribute("aria-expanded"), "false", "menu keys never toggle the row");

    // Pinning targets only that artifact and does not expand it.
    const pinMenu = await openMenu(browser, "b-docs");
    assert.equal(await docs.getAttribute("aria-expanded"), "false");
    await screenshot(browser, "03-menu-noninstallable");
    await pinMenu.$("button=固定").click();
    await browser.waitUntil(async () => (await item(browser, "b-docs").getAttribute("data-pinned")) === "true");
    assert.deepEqual((await calls(browser)).at(-1), ["pin", "b-docs", true]);
    assert.equal(await docs.getAttribute("aria-expanded"), "false");
    assertAligned(await measure(browser), { meta: true });

    // Install keeps the existing dialog flow for an artifact with a firmware package.
    const installMenu = await openMenu(browser, "c-bk");
    assert.deepEqual(
      (await menuItems(browser, installMenu)).map(({ text, disabled }) => [text, disabled]),
      [
        ["安装", false],
        ["固定", false],
        ["删除", false],
      ],
    );
    await installMenu.$("button=安装").click();
    const install = await browser.$('dialog[aria-label="安装到设备"]');
    await install.waitForDisplayed();
    await install.$("button=取消").click();
    await install.waitForDisplayed({ reverse: true });

    // Deletion confirms first; cancelling keeps the artifact, confirming removes only it.
    for (const confirm of [false, true]) {
      const menu = await openMenu(browser, "d-logs");
      await menu.$("button=删除").click();
      const dialog = await browser.$('dialog[aria-label="删除产物"]');
      await dialog.waitForDisplayed();
      assert.match(await dialog.getText(), /Export crash logs/);
      await screenshot(browser, confirm ? "05-delete-confirm" : "04-delete-cancel");
      await dialog.$(confirm ? ".button.danger" : "button=取消").click();
      await dialog.waitForDisplayed({ reverse: true });
      if (!confirm) {
        assert.equal(await item(browser, "d-logs").isExisting(), true);
        assert.equal(
          (await calls(browser)).some(([kind]) => kind === "delete"),
          false,
        );
      }
    }
    await item(browser, "d-logs").waitForExist({ reverse: true });
    assert.deepEqual(
      (await calls(browser)).filter(([kind]) => kind === "delete"),
      [["delete", "d-logs"]],
    );
    assert.equal((await browser.$$(".compile-artifact-item")).length, 7);
    await screenshot(browser, "06-after-delete");

    // The last row's menu fits inside a short window.
    await browser.setWindowSize(1180, 560);
    await browser.execute(() =>
      document.querySelector(".compile-artifact-item:last-child").scrollIntoView({ block: "end" }),
    );
    await pause(150);
    const lastMenu = await openMenu(browser, "h-map");
    const fit = await browser.execute((menu) => {
      const rect = menu.getBoundingClientRect();
      const button = document.querySelector('#artifact-h-map button[aria-haspopup="menu"]').getBoundingClientRect();
      return {
        top: rect.top,
        bottom: rect.bottom,
        left: rect.left,
        right: rect.right,
        button: button.top,
        height: innerHeight,
        width: innerWidth,
        scroll: menu.scrollHeight - menu.clientHeight,
      };
    }, lastMenu);
    assert.ok(fit.top >= 0 && fit.bottom <= fit.height, `last-row menu inside the viewport ${JSON.stringify(fit)}`);
    assert.ok(fit.left >= 0 && fit.right <= fit.width);
    assert.ok(fit.bottom <= fit.button, "the last-row menu opens above its trigger");
    await screenshot(browser, "07-menu-last-row");
    await browser.keys("Escape");

    // A medium window keeps every column aligned.
    await browser.setWindowSize(760, 760);
    await hideSidebarOverlay(browser);
    assertAligned(await measure(browser), { meta: true });
    await screenshot(browser, "08-medium");

    // Narrow window: file summaries yield to titles, times and menus stay aligned.
    await browser.setWindowSize(600, 760);
    await hideSidebarOverlay(browser);
    const narrow = await measure(browser);
    assert.ok(narrow.list.width < 560, `container is narrow (${narrow.list.width}px)`);
    assertAligned(narrow, { meta: false });
    await screenshot(browser, "09-narrow");
    await row(browser, "a-long").click();
    await browser.$("#artifact-a-long .compile-artifact-details").waitForDisplayed();
    assert.ok((await measure(browser)).overflow <= 0);
    await screenshot(browser, "10-narrow-expanded");

    // English labels follow the App language.
    await browser.setWindowSize(1180, 820);
    await browser.url(`${origin}/test/browser/artifact-list.html?demo=1&lang=en`);
    await openArtifacts(browser, "Compiling");
    await item(browser, "h-map").waitForExist({ timeout: 20000 });
    assert.equal(await trigger(browser, "h-map").getAttribute("aria-label"), "Manage Linker map only");
    const english = await openMenu(browser, "h-map");
    assert.deepEqual(
      (await menuItems(browser, english)).map(({ text, disabled }) => [text, disabled]),
      [
        ["Install", true],
        ["Pin", false],
        ["Delete", false],
      ],
    );
    assert.equal(
      (await menuItems(browser, english))[0].title,
      "This artifact has no installable firmware package (.update.tar.zlib).",
    );
  } catch (error) {
    error.message += `\n${log()}`;
    throw error;
  } finally {
    await browser?.deleteSession().catch(() => {});
    server.kill();
  }
});
