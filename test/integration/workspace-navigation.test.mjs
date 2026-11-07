import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { waitFor, withApp } from "./native-app.mjs";

test("native breadcrumbs navigate siblings and save edits with titlebar feedback", { timeout: 90000 }, (t) =>
  withApp(t, async (app) => {
    for (const [path, content] of Object.entries({
      "alpha/game.lua": "return 42\n",
      "alpha/helper.lua": "return 7\n",
      "alpha/assets/sprite.json": '{"frame":1}\n',
    }))
      assert.equal((await app.invoke("project_write", { id: "alpha", path, content })).ok, true);
    await app.openProject("alpha");
    await app.idle();
    await app.browser.$('button[aria-label="显示代码"]').click();
    await app.browser.$(".cm-content").waitForDisplayed();
    const current = () => app.browser.$('.code-breadcrumbs button[aria-current="page"]');
    await current().waitForDisplayed();
    // A pointer click on macOS does not automatically focus buttons.
    await app.browser.execute(() => {
      document.activeElement?.blur();
      document.querySelector(".code-breadcrumbs button").click();
    });
    const menu = () => app.browser.$("[data-breadcrumb-menu]:popover-open");
    await menu().waitForDisplayed();
    assert.equal(await app.browser.execute(() => document.activeElement?.getAttribute("role")), "tree");
    const row = (path) => menu().$(`[role="treeitem"][data-path=${JSON.stringify(path)}]`);
    await row("alpha").click();
    await row("alpha/game.lua").click();
    await waitFor(async () => (await current().getText()) === "game.lua", "breadcrumb file to open");
    assert.equal(await app.browser.$(".workspace-authoring .editor-status").isExisting(), false);
    await current().click();
    assert.deepEqual(
      await app.browser.execute(() =>
        [...document.querySelectorAll('[data-breadcrumb-menu]:popover-open [role="treeitem"]')].map(
          (row) => row.dataset.path,
        ),
      ),
      ["alpha/assets", "alpha/game.lua", "alpha/helper.lua"],
    );
    assert.equal(
      await app.browser.execute(() => document.activeElement?.getAttribute("role")),
      "tree",
      "pointer opening keeps keyboard focus in the file tree",
    );
    await app.browser.keys("Home");
    await app.browser.keys("ArrowRight");
    await app.browser.keys("ArrowRight");
    assert.equal(
      await app.browser.execute(() => {
        const tree = document.querySelector("[data-breadcrumb-menu]:popover-open");
        return document.getElementById(tree.getAttribute("aria-activedescendant"))?.dataset.path;
      }),
      "alpha/assets/sprite.json",
    );
    await app.screenshot("workspace-breadcrumbs-native");
    await app.browser.keys("Escape");
    assert.equal(await current().getAttribute("aria-expanded"), "false");
    assert.equal(await app.browser.execute(() => document.activeElement?.getAttribute("aria-current")), "page");
    const editor = await app.browser.$(".cm-content");
    await editor.click();
    await app.browser.keys(["META", "END"]);
    await app.browser.keys("\n-- native navigation save sentinel");
    await current().click();
    await row("alpha/helper.lua").click();
    await waitFor(async () => (await current().getText()) === "helper.lua", "sibling file to open after save");
    const saved = await readFile(join(app.data, "projects", "alpha", "alpha", "game.lua"), "utf8");
    assert.match(saved, /native navigation save sentinel/);
    await waitFor(() => app.browser.execute(() => !document.querySelector(".editor-status")), "idle titlebar to clear");
    assert.equal(await app.browser.$("select").isExisting(), false);
    assert.equal(app.model.requests.length, 0, "navigation never sends a model request");
  }),
);
