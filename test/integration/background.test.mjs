import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { release } from "node:os";
import { promisify } from "node:util";
import test from "node:test";
import { withApp } from "./native-app.mjs";

const execute = promisify(execFile);

test(
  "native macOS tests interact with hidden windows without activating the app",
  { skip: process.platform !== "darwin" || process.env.GIZMOTH_E2E_FOREGROUND === "1", timeout: 90000 },
  (t) =>
    withApp(t, async (app) => {
      const assertBackground = async (label) => {
        for (const command of ["is_visible", "is_focused"]) {
          const result = await app.invoke(`plugin:window|${command}`, { label });
          assert.equal(result.ok, true);
          assert.equal(result.value, false, `${label} must remain ${command === "is_visible" ? "hidden" : "unfocused"}`);
        }
        // AppKit inspection needs no Accessibility or System Events permission.
        const { stdout } = await execute("osascript", [
          "-l", "JavaScript", "-e",
          "ObjC.import('AppKit'); Number($.NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier)",
        ]);
        assert.notEqual(Number(stdout.trim()), app.pid, "The fixture must not become the foreground application");
      };

      await assertBackground("main");
      await app.openProject("alpha");
      await app.browser.$("aside.agent textarea").setValue("Typing into a hidden native webview");
      assert.equal(await app.browser.$("aside.agent textarea").getValue(), "Typing into a hidden native webview");
      await assertBackground("main");
      await app.screenshot("background-main");

      await t.test(
        "simulator windows stay hidden when opened and reopened",
        {
          // Darwin 23 is macOS 14, the first version with isolated UUID stores.
          skip: Number.parseInt(release(), 10) < 23 && "Simulator isolation requires macOS 14 or newer",
        },
        async () => {
          const profile = { id: randomUUID(), name: "Background device", model: "h106-tiga", firmwareId: "", view: "screen" };
          assert.equal((await app.invoke("simulator_profile_create", { profile })).ok, true);
          const token = randomUUID();
          const opened = await app.invoke("open_simulator", { profileId: profile.id, token });
          assert.equal(opened.ok, true);
          const label = opened.value;
          await app.browser.switchToWindow(label);
          await app.browser.$(".game-preview").waitForDisplayed({ timeout: 30000 });
          await assertBackground(label);
          await app.screenshot("background-simulator");
          await app.browser.switchToWindow("main");
          assert.deepEqual(await app.invoke("open_simulator", { profileId: profile.id, token }), opened);
          await assertBackground(label);
          await assertBackground("main");
        },
      );
    }),
);
