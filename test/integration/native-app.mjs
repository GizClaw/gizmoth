import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { remote } from "webdriverio";
import { startMockModel } from "./mock-model.mjs";

export const root = dirname(dirname(dirname(fileURLToPath(import.meta.url))));
export const artifacts = join(root, "src-tauri", "target", "native-e2e-results");
const executable = join(
  root,
  "src-tauri",
  "target",
  "native-e2e",
  "debug",
  process.platform === "win32" ? "gizmoth.exe" : "gizmoth",
);
export const projectNames = { alpha: "E2E Alpha", beta: "E2E Beta" };

function simulatorRuntimeSnapshot() {
  const child = document.querySelector("iframe")?.contentWindow;
  const pixel = (source) => {
    if (!source) return null;
    const sample = document.createElement("canvas");
    sample.width = sample.height = 1;
    const context = sample.getContext("2d");
    context.drawImage(source, 10, 10, 1, 1, 0, 0, 1, 1);
    return [...context.getImageData(0, 0, 1, 1).data];
  };
  return {
    url: location.href,
    framebuffer: {
      source: pixel(child?.document.querySelector("canvas#canvas")),
      preview: pixel(document.querySelector("canvas.preview-screen")),
    },
    visibility: document.visibilityState,
    ready: document.readyState,
    tauri: !!window.__TAURI_INTERNALS__,
    boot: window.__gizmothBootTrace ?? null,
    resources: performance
      .getEntriesByType("resource")
      .slice(-20)
      .map((entry) => ({
        name: entry.name,
        type: entry.initiatorType,
        duration: entry.duration,
        bytes: entry.transferSize,
      })),
    runtimeState: document.querySelector(".preview-state")?.textContent ?? null,
    displayBrightness: child?.Module?.h2WebOutputs?.display?.brightnessPercent ?? null,
    pressed: [...document.querySelectorAll(".game-key.pressed")].map((button) => button.getAttribute("aria-label")),
    firmware: child?.h2WebState ?? null,
    control: child?.Module?.h2GizClaw ? { closed: child.Module.h2GizClaw.closed } : null,
    trace: window.__simulatorTrace ?? null,
  };
}

export async function waitFor(check, message, timeout = 12000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await check()) return;
    await pause(50);
  }
  throw new Error(`Timed out: ${message}`);
}

/** Windows WDIO 1.5.1 caches async message handlers by label, even after that
 * WebView has been destroyed. Observe the real callback through sync inspection
 * instead, so recreating a fixed device never reuses a stale native handler. */
export async function runNativeAsync(browser, script, ...args) {
  if (process.platform !== "win32") return browser.executeAsync(script, ...args);
  const key = `__gizmothNativeAsync_${randomUUID().replaceAll("-", "")}`;
  await browser.execute(`(() => {
    const key = ${JSON.stringify(key)};
    window[key] = { done: false };
    let settled = false;
    const done = value => { if (!settled) { settled = true; window[key] = { done: true, value }; } };
    const fail = error => { if (!settled) { settled = true; window[key] = { done: true, exception: String(error) }; } };
    try {
      const pending = (${script.toString()}).apply(null, [...${JSON.stringify(args)}, done]);
      if (pending && typeof pending.catch === 'function') pending.catch(fail);
    } catch (error) { fail(error); }
  })()`);
  let result;
  try {
    await waitFor(
      async () => {
        result = await browser.execute((key) => window[key] ?? null, key);
        return result?.done === true;
      },
      "native asynchronous callback",
      60000,
    );
    if (result.exception) throw new Error(result.exception);
    return result.value;
  } finally {
    await browser
      .execute((key) => {
        delete window[key];
      }, key)
      .catch(() => {});
  }
}

/** Choose through the HTML combobox and listbox, as a user would. */
export async function chooseSelect(app, label, value) {
  const combo = await app.browser.$(`button[role="combobox"][aria-label=${JSON.stringify(label)}]`);
  await combo.waitForEnabled();
  await combo.click();
  const id = await combo.getAttribute("aria-controls");
  const menu = await app.browser.$(`[id=${JSON.stringify(id)}]`);
  await menu.waitForDisplayed();
  const option = await menu.$(`[role="option"][data-value=${JSON.stringify(value)}]`);
  assert.notEqual(await option.getAttribute("aria-disabled"), "true");
  await option.click();
  await waitFor(async () => (await combo.getAttribute("data-value")) === value, `dropdown ${label} to select ${value}`);
}

/** Emulate the OS-provided WebView locale without changing the Mac's preferences. */
export async function setAppLocale(app, locale) {
  assert.equal((await app.invoke("set_language_preference", { preference: "system" })).ok, true);
  await app.browser.execute((locale) => {
    Object.defineProperty(navigator, "language", { configurable: true, value: locale });
    Object.defineProperty(navigator, "languages", { configurable: true, value: [locale] });
    window.dispatchEvent(new Event("languagechange"));
  }, locale);
}

async function unusedPort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function seed(data, modelUrl, history) {
  await mkdir(data, { recursive: true });
  await writeFile(join(data, ".gizmoth-native-e2e"), "Synthetic native integration test data only.\n");
  await writeFile(join(data, "devices.json"), "[]");
  await writeFile(join(data, "simulators.json"), "[]");
  await writeFile(join(data, "desktop.json"), JSON.stringify({ keepInTray: false, theme: "light", language: "zh-CN" }));
  await writeFile(
    join(data, "providers.json"),
    JSON.stringify([
      {
        id: "native-e2e",
        kind: "custom",
        name: "Synthetic loopback provider",
        api: "openai-completions",
        baseUrl: modelUrl,
        hasKey: false,
        models: [
          { id: "gpt-5.2", name: "E2E Reasoning", reasoning: true },
          { id: "plain-fixture", name: "E2E Plain", reasoning: false },
          { id: "another-reasoning", name: "E2E Other Reasoning", reasoning: true },
        ].map((model) => ({ ...model, images: false, contextWindow: 32768, maxTokens: 2048 })),
      },
    ]),
  );
  for (const [id, name] of Object.entries(projectNames)) {
    const project = join(data, "projects", id);
    await mkdir(project, { recursive: true });
    await writeFile(join(project, "app.json"), JSON.stringify({ app_id: id, display_name: name, version: "1.0.0" }));
    await writeFile(join(project, `${id}.lua`), `-- ${name} untouched sentinel\nreturn 1\n`);
  }
  await mkdir(join(data, "chats"), { recursive: true });
  await writeFile(join(data, "chats", "alpha.json"), JSON.stringify(history ?? []));
}

export async function startApp({ history, failSimulatorTracking = false, holdSimulatorOpening = false, failCloseSimulatorPid = false } = {}) {
  const temporary = await mkdtemp(join(tmpdir(), "gizmoth-native-e2e-"));
  const data = join(temporary, "app-data");
  const firmwareProbe = process.env.GIZMOTH_E2E_FIRMWARE_ARCHIVE ? join(data, "firmware-probe.archive") : null;
  // Home-folder tools see only this fake home, marked as owned by the harness.
  const home = join(temporary, "home");
  await mkdir(join(home, "Documents"), { recursive: true });
  await mkdir(join(home, ".ssh"), { recursive: true });
  await writeFile(join(home, ".gizmoth-native-e2e-home"), "");
  await writeFile(join(home, "Documents", "notes.md"), "home notes sentinel\n");
  await writeFile(join(home, ".ssh", "id_e2e"), "SECRET KEY MUST NOT LEAK\n");
  const model = await startMockModel();
  let child;
  let browser;
  let output = "";
  let spawnError;
  let closed = false;
  const killAtExit = () => child?.kill();
  process.once("exit", killAtExit);
  const api = {
    data,
    home,
    temporary,
    model,
    get browser() {
      return browser;
    },
    async close() {
      if (closed) return;
      closed = true;
      process.removeListener("exit", killAtExit);
      if (browser) {
        // UUID WebKit stores live outside the temporary metadata directory.
        // Remove only this fixture's registered devices before ending its driver.
        try {
          await browser.switchToWindow("main");
          await api.invoke("simulator_cancel_update");
          const profiles = await api.invoke("simulator_profiles_list");
          if (profiles.ok)
            for (const profile of profiles.value)
              await api.invoke("simulator_profile_delete", { id: profile.id }).catch(() => {});
        } catch {}
        await browser.deleteSession().catch(() => {});
      }
      if (child && child.exitCode === null && child.signalCode === null) {
        child.kill();
        await Promise.race([new Promise((resolve) => child.once("exit", resolve)), pause(3000)]);
        if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
      }
      await model.close();
      await rm(temporary, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    },
    async restart() {
      assert.ok(
        !(await browser.getWindowHandles()).some((label) => label.startsWith("simulator-")),
        "Close and flush simulators before restarting the fixture",
      );
      await browser.deleteSession();
      browser = undefined;
      if (child && child.exitCode === null && child.signalCode === null) {
        const exited = new Promise((resolve) => child.once("exit", resolve));
        child.kill();
        await Promise.race([exited, pause(3000)]);
        if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
      }
      await launch();
    },
    async screenshot(name) {
      await mkdir(artifacts, { recursive: true });
      const path = join(artifacts, `${name}.png`);
      const tracing = process.env.GIZMOTH_SIMULATOR_TRACE === "1";
      const before = tracing ? await browser.execute(simulatorRuntimeSnapshot).catch(() => null) : null;
      await browser.saveScreenshot(path);
      if (tracing) {
        const after = await browser.execute(simulatorRuntimeSnapshot).catch(() => null);
        console.error(
          JSON.stringify({
            event: "screenshot",
            name,
            before: before && {
              visibility: before.visibility,
              phase: before.runtimeState,
              control: before.control,
              events: before.trace?.length,
            },
            after: after && {
              visibility: after.visibility,
              phase: after.runtimeState,
              control: after.control,
              events: after.trace?.length,
            },
          }),
        );
      }
      return path;
    },
    async diagnostics(name) {
      await mkdir(artifacts, { recursive: true });
      const label = name.replace(/[^a-z0-9-]+/gi, "-").slice(0, 100);
      await writeFile(join(artifacts, `${label}.log`), output);
      await writeFile(
        join(artifacts, `${label}-requests.json`),
        JSON.stringify({ requests: model.requests, unexpected: model.unexpected }, null, 2),
      );
      if (browser) {
        const runtime = await browser.execute(simulatorRuntimeSnapshot).catch((error) => ({ failure: String(error) }));
        await writeFile(join(artifacts, `${label}-runtime.json`), JSON.stringify(runtime, null, 2));
        await api.screenshot(label).catch(() => {});
        const source = await browser.getPageSource().catch(() => "Could not retrieve native page source");
        await writeFile(join(artifacts, `${label}.html`), source);
      }
    },
    async openProject(id) {
      if (await browser.$("aside.agent").isExisting()) {
        await browser.$('button.nav-item[aria-label="项目"]').click();
      }
      const row = await browser.$(`button.project-row[aria-label^="${projectNames[id]}，"]`);
      await row.waitForDisplayed();
      await row.click();
      await browser.$("aside.agent textarea").waitForEnabled();
    },
    async send(text) {
      await browser.$("aside.agent textarea").setValue(text);
      const send = await browser.$('aside.agent button[aria-label="发送"]');
      await send.waitForEnabled();
      await send.click();
    },
    async busy() {
      await browser.waitUntil(async () => (await browser.$("aside.agent").getAttribute("aria-busy")) === "true", {
        timeoutMsg: "Native agent did not become busy",
      });
    },
    async idle() {
      await browser.waitUntil(
        () =>
          browser.execute(() => {
            const agent = document.querySelector("aside.agent");
            return (
              agent?.getAttribute("aria-busy") === "false" &&
              !agent.querySelector(".spin, .agent-run-status") &&
              !!agent.querySelector("button[aria-label='发送']")
            );
          }),
        { timeout: 12000, timeoutMsg: "Native agent did not settle or retained a spinner" },
      );
      assert.equal(await browser.$("aside.agent textarea").isEnabled(), true);
    },
    async invoke(command, args = {}) {
      return runNativeAsync(
        browser,
        (command, args, done) => {
          window.__TAURI__.core.invoke(command, args).then(
            (value) => done({ ok: true, value }),
            // The embedded driver's async bridge reserves an `error` result key.
            (error) => done({ ok: false, reason: String(error) }),
          );
        },
        command,
        args,
      );
    },
    async chat(id = "alpha") {
      const record = JSON.parse(await readFile(join(data, "chats", `${id}.json`), "utf8"));
      return Array.isArray(record) ? record : record.messages;
    },
    async brief(id = "alpha") {
      const record = JSON.parse(await readFile(join(data, "chats", `${id}.json`), "utf8"));
      return Array.isArray(record) ? "" : record.brief;
    },
  };
  async function launch() {
    spawnError = undefined;
    const port = await unusedPort();
    const environment = Object.fromEntries(
      Object.entries(process.env).filter(([key]) => !/(?:API_KEY|ACCESS_TOKEN|AUTH_TOKEN|SECRET_KEY)$/i.test(key)),
    );
    child = spawn(executable, [], {
      cwd: root,
      env: {
        ...environment,
        ...(firmwareProbe ? { GIZMOTH_E2E_FIRMWARE_ARCHIVE: firmwareProbe } : {}),
        GIZMOTH_E2E_DATA_DIR: data,
        GIZMOTH_E2E_HOME: home,
        GIZMOTH_E2E_FAIL_SIMULATOR_TRACKING: failSimulatorTracking ? "1" : "0",
        GIZMOTH_E2E_HOLD_SIMULATOR_OPENING: holdSimulatorOpening ? "1" : "0",
        GIZMOTH_E2E_FAIL_SIMULATOR_CLOSE_PID: failCloseSimulatorPid ? "1" : "0",
        TAURI_WEBDRIVER_PORT: String(port),
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const record = (chunk) => {
      output = (output + chunk.toString()).slice(-128000);
    };
    child.stdout.on("data", record);
    child.stderr.on("data", record);
    child.once("error", (error) => {
      spawnError = error;
    });
    await waitFor(
      async () => {
        if (spawnError) throw spawnError;
        if (child.exitCode !== null || child.signalCode !== null)
          throw new Error(`Native fixture exited before its driver was ready:\n${output}`);
        try {
          const response = await fetch(`http://127.0.0.1:${port}/status`, { signal: AbortSignal.timeout(500) });
          return response.ok && (await response.json()).value?.ready === true;
        } catch {
          return false;
        }
      },
      "embedded native WebDriver startup",
      30000,
    );
    browser = await remote({
      hostname: "127.0.0.1",
      port,
      capabilities: {},
      logLevel: "error",
      connectionRetryCount: 0,
      connectionRetryTimeout: 60000,
      waitforTimeout: 12000,
    });
    await browser.setTimeout({ implicit: 0, script: 60000, pageLoad: 20000 });
    await browser.$(".project-list").waitForDisplayed();
    assert.equal(await browser.execute(() => !!window.__TAURI_INTERNALS__ && !!window.__TAURI__?.core), true);
    assert.equal(await browser.$(".demo-indicator").isExisting(), false);
    const projects = await api.invoke("projects_list");
    assert.equal(projects.ok, true);
    assert.deepEqual(projects.value.map((project) => project.id).sort(), ["alpha", "beta"]);
  }
  try {
    await seed(data, model.url, history);
    if (firmwareProbe) await copyFile(process.env.GIZMOTH_E2E_FIRMWARE_ARCHIVE, firmwareProbe);
    await writeFile(join(temporary, "outside.txt"), "outside project sentinel\n");
    await launch();
    return api;
  } catch (error) {
    await api.diagnostics("startup-failure").catch(() => {});
    await api.close();
    throw error;
  }
}

export async function withApp(test, run, options) {
  const app = await startApp(options);
  try {
    await run(app);
    assert.deepEqual(app.model.unexpected, [], "Only explicitly scripted loopback model requests are allowed");
    assert.equal(
      app.model.pending(),
      0,
      "All expected model requests should have reached the real native HTTP transport",
    );
  } catch (error) {
    await app.diagnostics(test.name).catch(() => {});
    throw error;
  } finally {
    await app.close();
  }
}
