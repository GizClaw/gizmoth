import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { chooseSelect, setAppLocale, waitFor, withApp } from "./native-app.mjs";

async function received(plan) {
  await waitFor(() => !!plan.request, "the synthetic model request");
  return plan.request.body;
}

// This exercises the real agent, MCP, WebView CSP and project IPC. The model is scripted;
// no real provider credentials, device access or internet search are involved.
test(
  "built-in GizOS MCP supplies offline device APIs before the agent writes and validates a Lua app",
  { timeout: 90000 },
  (t) =>
    withApp(t, async (app) => {
      assert.equal(
        (
          await app.invoke("agent_permissions_set", {
            id: "alpha",
            permissions: { files: "none", run: "deny", web: "deny" },
          })
        ).ok,
        true,
      );
      await app.openProject("alpha");
      await app.idle();
      const docs = app.model.enqueue({
        tools: [
          { id: "docs-search", name: "gizos_docs_search", arguments: { query: "绘图 按键 计时", limit: 3 } },
          { id: "docs-example", name: "gizos_docs_read", arguments: { topic: "input-example" } },
          { id: "docs-english", name: "gizos_docs_read", arguments: { topic: "gizos-overview", language: "en" } },
          { id: "docs-chinese", name: "gizos_docs_read", arguments: { topic: "gizos-overview", language: "zh-CN" } },
        ],
      });
      const source = `local runtime = require("runtime")
local display = require("display")
local system = require("system")
local active = false
local ok = tonumber(args and args.ok)
if ok then
  runtime.components.on(ok, runtime.event.BUTTON_UP, function()
    active = not active
  end)
end
while true do
  local started = system.millis()
  display.clear("black")
  display.fill_rect(8, 8, 40, 40, active and "green" or "blue")
  display.draw_text(8, 56, "Press OK", { font_size = 14 })
  display.present()
  runtime.sleep(math.max(1, 33 - (system.millis() - started)))
end
`;
      const edits = app.model.enqueue({
        tools: [
          { id: "write", name: "write_file", arguments: { path: "alpha.lua", content: source } },
          { id: "check", name: "check_package", arguments: {} },
        ],
      });
      const final = app.model.enqueue({
        text: "Device APIs came from the bundled GizOS docs; package validation passed.",
      });
      await app.send(
        "Read the bundled GizOS documentation, then make a small screen app with an OK button and a timed frame loop.",
      );
      const first = await received(docs);
      const advertised = first.tools.map((tool) => tool.function.name);
      assert.ok(advertised.includes("gizos_docs_search"));
      assert.ok(advertised.includes("gizos_docs_read"));
      assert.match(JSON.stringify(first.messages), /gizos-docs/);
      assert.doesNotMatch(JSON.stringify(first.messages), /unless the user tells you a device API exists/);
      const afterDocs = await received(edits);
      assert.match(
        afterDocs.messages.find((message) => message.tool_call_id === "docs-english").content,
        /portable firmware and application platform/,
      );
      assert.match(afterDocs.messages.find((message) => message.tool_call_id === "docs-chinese").content, /可移植平台/);
      const example = afterDocs.messages.find((message) => message.tool_call_id === "docs-example");
      assert.match(example.content, /display\.draw_text/);
      assert.match(example.content, /runtime\.components\.on/);
      assert.match(example.content, /runtime\.sleep/);
      assert.match(example.content, /75deb77229d3fb07dd10d39683c69009b1eb45e9/);
      const afterEdits = await received(final);
      const check = afterEdits.messages.find((message) => message.tool_call_id === "check");
      assert.match(check.content, /OK: alpha/);
      await app.idle();
      assert.equal(await readFile(join(app.data, "projects", "alpha", "alpha.lua"), "utf8"), source);
      assert.equal((await app.browser.$$('aside.agent .tool[data-state="success"]')).length, 6);
      await setAppLocale(app, "zh-CN");
      await app.browser.execute(() => localStorage.setItem("gizmoth.docs.language", "en"));
      await app.browser.$('button.nav-item[aria-label="文档"]').click();
      await app.browser.$(".docs-reader").waitForDisplayed();
      assert.match(await app.browser.$(".docs-markdown").getText(), /GizOS/);
      await chooseSelect(app, "文档栏目", "lua");
      await app.browser.$('button.docs-topic[title^="Complete Lua example"]').waitForDisplayed();
      await app.browser.$('button.docs-topic[title^="Complete Lua example"]').click();
      assert.match(await app.browser.$(".docs-markdown").getText(), /display\.present/);
      assert.equal(app.model.requests.length, 3);
      await app.screenshot("gizos-offline-documentation-reader");
      assert.equal(await app.browser.$('[aria-label="文档语言 / Documentation language"]').isExisting(), false);
      assert.equal(await app.browser.execute(() => localStorage.getItem("gizmoth.docs.language")), null);
      await chooseSelect(app, "文档栏目", "start");
      for (const [locale, language, text] of [
        ["en-US", "en", /portable firmware and application platform/],
        ["zh-Hant-TW", "zh-CN", /可移植平台/],
        ["fr-FR", "en", /portable firmware and application platform/],
      ]) {
        await setAppLocale(app, locale);
        await waitFor(
          async () => (await app.browser.$(".documentation-layout").getAttribute("lang")) === language,
          `documentation locale ${locale}`,
        );
        assert.match(await app.browser.$(".docs-markdown").getText(), text);
      }
      assert.equal(app.model.requests.length, 3, "system language changes require no model request");
    }),
);
