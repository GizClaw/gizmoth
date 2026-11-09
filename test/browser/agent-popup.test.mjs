import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { setTimeout as pause } from "node:timers/promises";
import test from "node:test";
import { remote } from "webdriverio";

// Run in a standalone Chrome session: embedded preview panes may reject every popup.
// This server is owned by the test and is always terminated during teardown.
test("allow-mode simulator opening obtains a fresh browser gesture", { timeout: 150000 }, async () => {
  const origin = "http://127.0.0.1:1420";
  const server = spawn(
    process.execPath,
    ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", "1420", "--strictPort"],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  let log = "";
  const collect = (chunk) => {
    log = (log + chunk.toString()).slice(-12000);
  };
  server.stdout.on("data", collect);
  server.stderr.on("data", collect);
  let browser;
  try {
    const deadline = Date.now() + 45000;
    while (true) {
      if (server.exitCode !== null || server.signalCode !== null)
        throw new Error(`Vite exited ${server.exitCode ?? server.signalCode}: ${log}`);
      try {
        const response = await fetch(`${origin}/test/browser/agent-popup.html`, { signal: AbortSignal.timeout(1000) });
        if (response.ok) break;
      } catch {
        /* Wait until this test's server accepts requests. */
      }
      if (Date.now() >= deadline) throw new Error(`Vite did not become ready: ${log}`);
      await pause(250);
    }
    browser = await remote({
      logLevel: "error",
      capabilities: {
        browserName: "chrome",
        "goog:chromeOptions": {
          args: ["--headless=new", "--no-sandbox", "--disable-dev-shm-usage"],
          // ChromeDriver otherwise disables popup blocking, masking this regression.
          excludeSwitches: ["disable-popup-blocking"],
        },
      },
    });
    await browser.setWindowSize(1000, 800);
    await browser.url(`${origin}/test/browser/agent-popup.html?demo=1`);
    await browser.$("#start").waitForEnabled({ timeout: 20000 });
    await browser.$("#start").click();
    await browser.$("#allow-open").waitForDisplayed({ timeout: 20000 });
    await browser.$("#allow-open").click();
    await browser.waitUntil(
      async () =>
        browser.execute(() => {
          const result = JSON.parse(localStorage.getItem("gizmoth.test.agent-popup") ?? "null");
          return result?.stage === "pass" || result?.stage === "fail";
        }),
      { timeout: 15000, timeoutMsg: "Popup admission did not settle" },
    );
    const result = await browser.execute(() => JSON.parse(localStorage.getItem("gizmoth.test.agent-popup")));
    assert.equal(result.stage, "pass", result.message);
    assert.equal(result.confirmed, true);
    assert.equal(result.active, true);
    assert.equal(result.opened, true, "the real popup is admitted with popup blocking enabled");
  } catch (error) {
    if (browser)
      console.error(
        "Popup fixture result:",
        await browser.execute(() => localStorage.getItem("gizmoth.test.agent-popup")).catch(() => null),
      );
    throw error;
  } finally {
    await browser?.deleteSession().catch(() => {});
    if (server.exitCode === null && server.signalCode === null) {
      const exited = new Promise((resolve) => server.once("exit", resolve));
      server.kill("SIGTERM");
      const force = setTimeout(() => server.kill("SIGKILL"), 5000);
      await exited;
      clearTimeout(force);
    }
  }
});
