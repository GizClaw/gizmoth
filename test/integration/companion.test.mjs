import assert from "node:assert/strict";
import test from "node:test";
import { waitFor, withApp } from "./native-app.mjs";

const mounted = () => document.querySelectorAll("peetty-companion").length;

test(
  "native page companion survives navigation and persists hide/restore without model requests",
  { timeout: 90000 },
  (t) =>
    withApp(t, async (app) => {
      await waitFor(() => app.browser.execute(mounted), "the local companion mounts");
      assert.equal(await app.browser.execute(mounted), 1, "StrictMode leaves one companion");
      assert.equal(
        await app.browser.execute(() => !!document.querySelector("peetty-companion").shadowRoot.querySelector("svg")),
        true,
      );
      await app.browser.$('button[data-page="settings"]').click();
      const toggle = await app.browser.$('input[role="switch"][aria-label="页面小人"]');
      await toggle.waitForEnabled();
      assert.equal(await toggle.isSelected(), true);
      assert.equal(await app.browser.execute(mounted), 1, "navigation preserves the instance");

      const foreground = await app.browser.execute(() => !document.hidden);
      if (foreground) {
        await waitFor(
          () =>
            app.browser.execute(() => {
              const host = document.querySelector("peetty-companion");
              return getComputedStyle(host).visibility === "visible" && getComputedStyle(host).display !== "none";
            }),
          "the companion finds a visible perch before keyboard interaction",
        );
        const keyboard = await app.browser.execute(() => {
          const root = document.querySelector("peetty-companion").shadowRoot;
          const figure = root.querySelector(".pet");
          const menu = root.querySelector(".menu");
          figure.focus();
          figure.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
          const opened = menu.classList.contains("show") && root.activeElement === menu.querySelector("button");
          menu.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
          return { opened, closed: !menu.classList.contains("show") && root.activeElement === figure };
        });
        assert.deepEqual(keyboard, { opened: true, closed: true });

        const action = await app.browser.execute(() => {
          const root = document.querySelector("peetty-companion").shadowRoot;
          const figure = root.querySelector(".pet");
          const menu = root.querySelector(".menu");
          figure.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
          const move = menu.querySelector("button:nth-child(2)");
          move.focus();
          move.click();
          return { closed: !menu.classList.contains("show"), restored: root.activeElement === figure };
        });
        assert.deepEqual(action, { closed: true, restored: true }, "an action closes the menu and returns focus");

        await app.browser.execute(() => {
          const root = document.querySelector("peetty-companion").shadowRoot;
          const figure = root.querySelector(".pet");
          const menu = root.querySelector(".menu");
          figure.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
          menu.dispatchEvent(new PointerEvent("pointerout", { bubbles: true }));
        });
        await app.browser.pause(850);
        assert.equal(
          await app.browser.execute(() =>
            document.querySelector("peetty-companion").shadowRoot.querySelector(".menu").classList.contains("show"),
          ),
          true,
          "a pending pointer-out timer keeps the focused menu positioned",
        );

        const tabOut = await app.browser.execute(() => {
          const root = document.querySelector("peetty-companion").shadowRoot;
          const figure = root.querySelector(".pet");
          const menu = root.querySelector(".menu");
          figure.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
          menu.querySelector("button:nth-child(2)").focus();
          const stayedOpen = menu.classList.contains("show");
          document.querySelector('input[role="switch"][aria-label="页面小人"]').focus();
          return { stayedOpen, closed: !menu.classList.contains("show") };
        });
        assert.deepEqual(tabOut, { stayedOpen: true, closed: true }, "focus leaving the menu closes it");
      } else {
        assert.equal(
          await app.browser.execute(() => getComputedStyle(document.querySelector("peetty-companion")).display),
          "none",
          "hidden windows stop rendering the companion",
        );
      }

      await toggle.click();
      await waitFor(async () => !(await app.browser.execute(mounted)), "the companion is destroyed on disable");
      await app.browser.refresh();
      await app.browser.$('button[data-page="settings"]').click();
      const restoredToggle = await app.browser.$('input[role="switch"][aria-label="页面小人"]');
      await restoredToggle.waitForEnabled();
      assert.equal(await restoredToggle.isSelected(), false, "hiding survives a native page reload");
      assert.equal(await app.browser.execute(mounted), 0);
      await restoredToggle.click();
      await waitFor(() => app.browser.execute(mounted), "the companion can be restored");

      await app.browser.execute(() =>
        document.querySelector("peetty-companion").shadowRoot.querySelector(".menu button:last-child").click(),
      );
      await waitFor(
        async () => !(await app.browser.execute(mounted)),
        "the SDK Hide action also disables the app setting",
      );
      assert.equal(await restoredToggle.isSelected(), false);
      await restoredToggle.click();
      await waitFor(() => app.browser.execute(mounted), "restoring after SDK Hide creates one fresh instance");
      assert.equal(await app.browser.execute(mounted), 1);
      if (foreground) {
        await waitFor(
          () =>
            app.browser.execute(() => {
              const host = document.querySelector("peetty-companion");
              const figure = host.shadowRoot.querySelector(".pet");
              const rect = figure.getBoundingClientRect();
              return (
                getComputedStyle(host).visibility === "visible" &&
                rect.top > 52 &&
                rect.bottom <= innerHeight &&
                !figure.classList.contains("gone")
              );
            }),
          "the character enters the visible page",
        );
        await app.screenshot("page-companion-native");
      }
      assert.equal(app.model.requests.length, 0);
    }),
);
