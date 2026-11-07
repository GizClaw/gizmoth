import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { chooseSelect, waitFor, withApp } from "./native-app.mjs";
import "./gizos-docs.test.mjs";
import "./select.test.mjs";
import "./workspace-navigation.test.mjs";
import "./appearance.test.mjs";

const options = { timeout: 90000 };
const modelTrigger = 'aside.agent button[aria-label^="选择模型："]';

async function thinkingValue(app) {
  return app.browser.$(modelTrigger).getAttribute("data-thinking");
}

async function thinkingChoices(app) {
  return app.browser.execute(() =>
    [...document.querySelectorAll('aside.agent [role="menu"][aria-label="思考程度"] [role="menuitemradio"]')].map(
      (item) => item.dataset.value,
    ),
  );
}

async function received(plan) {
  await waitFor(() => !!plan.request, "the queued request to reach the synthetic native model endpoint");
  return plan.request.body;
}

async function selectThinking(app, level) {
  // Effort lives in the model menu: open it, open the effort submenu, pick a level.
  await app.browser.$(modelTrigger).click();
  const row = await app.browser.$('aside.agent button[aria-haspopup="menu"][aria-label^="思考程度"]');
  await row.waitForDisplayed();
  assert.equal(await row.isEnabled(), true, "The effort row should be enabled for a reasoning model");
  await row.click();
  const item = await app.browser.$(`aside.agent [role="menuitemradio"][data-value="${level}"]`);
  await item.waitForDisplayed();
  await item.click();
  await app.browser.$(modelTrigger).waitForEnabled();
  assert.equal(await thinkingValue(app), level);
}

async function pressEscapeToStop(app) {
  // There is no Stop button: Esc stops while the run status shows its hint.
  assert.equal(await app.browser.$('aside.agent button[aria-label="停止"]').isExisting(), false);
  await app.browser.$("aside.agent .agent-stop-hint").waitForDisplayed();
  await app.browser.keys("Escape");
}

async function clearConversation(app) {
  await app.browser.$('button[aria-label="清空对话"]').click();
  const confirm = await app.browser.$('//dialog//button[normalize-space()="清空"]');
  await confirm.waitForDisplayed();
  await confirm.click();
  await app.idle();
  await waitFor(async () => (await app.chat()).length === 0, "the cleared conversation to be saved");
}

async function selectModel(app, name) {
  await app.browser.$(modelTrigger).click();
  const option = await app.browser.$(`div[role="option"][title^="${name}"]`);
  await option.waitForDisplayed();
  await option.click();
  await app.browser.$(modelTrigger).waitForEnabled();
}

test("native completion settles thinking, saves history and accepts the next prompt", options, (t) =>
  withApp(t, async (app) => {
    await app.openProject("alpha");
    await app.idle();
    const first = app.model.enqueue({
      hold: true,
      thinking: "E2E thinking is complete.",
      text: "First native answer.",
    });
    await app.send("First synthetic prompt");
    await received(first);
    await app.busy();
    assert.equal(await app.browser.$("aside.agent .agent-run-status").isExisting(), true);
    first.release();
    await app.idle();
    assert.match(await app.browser.$("aside.agent").getText(), /First native answer\./);
    assert.equal(
      await app.browser.execute(() => {
        const thinking = [...document.querySelectorAll("aside.agent details")].find((element) =>
          element.textContent.includes("E2E thinking is complete."),
        );
        return !!thinking && !thinking.open;
      }),
      true,
      "Completed thinking should be a collapsed disclosure",
    );
    assert.match(JSON.stringify(await app.chat()), /First native answer\./);

    const second = app.model.enqueue({ text: "Second native answer." });
    await app.send("Second synthetic prompt");
    await received(second);
    await app.idle();
    assert.match(JSON.stringify(await app.chat()), /Second native answer\./);
    assert.equal(await app.browser.$('button[aria-label="清空对话"]').isEnabled(), true);
    await app.screenshot("completion-thinking-idle");
    await clearConversation(app);
    assert.deepEqual(await app.chat(), []);
  }),
);

test("native model errors clear busy indicators and recover send and reset controls", options, (t) =>
  withApp(t, async (app) => {
    await app.openProject("alpha");
    const failed = app.model.enqueue({ error: "Synthetic E2E model failure" });
    await app.send("Fail this synthetic request");
    await received(failed);
    await app.idle();
    assert.match(await app.browser.$("aside.agent").getText(), /Synthetic E2E model failure/);
    assert.equal(await app.browser.$('button[aria-label="清空对话"]').isEnabled(), true);
    const recovery = app.model.enqueue({ text: "Recovered from synthetic failure." });
    await app.send("Recover after the synthetic failure");
    await received(recovery);
    await app.idle();
    assert.match(await app.browser.$("aside.agent").getText(), /Recovered from synthetic failure\./);
  }),
);

test("native Stop aborts the held HTTP stream and recovers for the next prompt", options, (t) =>
  withApp(t, async (app) => {
    await app.openProject("alpha");
    const held = app.model.enqueue({
      hold: true,
      thinking: "Waiting for Stop.",
      text: "This cancelled text must never appear.",
    });
    await app.send("Hold this synthetic response until stopped");
    await received(held);
    await app.busy();
    await pressEscapeToStop(app);
    await app.idle();
    await waitFor(() => held.disconnected === true, "Stop to cancel the real native HTTP response");
    held.release();
    assert.doesNotMatch(await app.browser.$("aside.agent").getText(), /This cancelled text must never appear/);
    const recovery = app.model.enqueue({ text: "Recovered after native Stop." });
    await app.send("A new synthetic prompt after Stop");
    await received(recovery);
    await app.idle();
    assert.match(await app.browser.$("aside.agent").getText(), /Recovered after native Stop\./);
  }),
);

test("native Stop cancels an SSE response with no body chunks or heartbeats", options, (t) =>
  withApp(t, async (app) => {
    await app.openProject("alpha");
    const silent = app.model.enqueue({
      hold: true,
      silent: true,
      heartbeat: false,
      text: "Silent cancelled response must never appear.",
    });
    await app.send("Wait silently without sending any model tokens");
    await received(silent);
    await app.busy();
    await pressEscapeToStop(app);
    await app.idle();
    await waitFor(
      () => silent.disconnected === true,
      "Stop to cancel native HTTP without waiting for another SSE chunk",
      5000,
    );
    silent.release();
    assert.doesNotMatch(await app.browser.$("aside.agent").getText(), /Silent cancelled response must never appear/);
    const recovery = app.model.enqueue({ text: "Recovered after a silent response was cancelled." });
    await app.send("Recover after silently waiting");
    await received(recovery);
    await app.idle();
    assert.match(await app.browser.$("aside.agent").getText(), /Recovered after a silent response was cancelled/);
  }),
);

test("reasoning high reaches native HTTP, survives reload and model changes, and stays project scoped", options, (t) =>
  withApp(t, async (app) => {
    await app.openProject("alpha");
    const defaultThinking = await thinkingValue(app);
    assert.notEqual(defaultThinking, "high", "A fresh fixture should start at its model's default level");
    await selectThinking(app, "high");
    const first = app.model.enqueue({ text: "High reasoning captured." });
    await app.send("Capture the selected reasoning level");
    assert.equal((await received(first)).reasoning_effort, "high");
    await app.idle();

    await selectModel(app, "E2E Other Reasoning");
    assert.equal(await thinkingValue(app), "high");
    await selectModel(app, "E2E Plain");
    assert.equal(await thinkingValue(app), "off");
    assert.deepEqual(await thinkingChoices(app), ["off"]);
    await selectModel(app, "E2E Reasoning");
    assert.equal(await thinkingValue(app), "high");

    await app.browser.refresh();
    await app.openProject("alpha");
    assert.equal(await thinkingValue(app), "high");
    const restored = app.model.enqueue({ text: "Restored high reasoning captured." });
    await app.send("Capture reasoning after reload");
    assert.equal((await received(restored)).reasoning_effort, "high");
    await app.idle();
    await app.openProject("beta");
    await app.idle();
    assert.equal(await thinkingValue(app), defaultThinking);
    await app.openProject("alpha");
    await app.idle();
    assert.equal(await thinkingValue(app), "high");
  }),
);

const orphanHistory = [
  { role: "user", content: [{ type: "text", text: "An interrupted historical tool request" }], timestamp: 1 },
  {
    role: "assistant",
    api: "openai-completions",
    provider: "native-e2e",
    model: "gpt-5.2",
    timestamp: 2,
    content: [
      {
        type: "toolCall",
        id: "orphan-historical-tool",
        name: "write_file",
        arguments: { path: "alpha/orphan.lua", content: "return 0\n" },
      },
    ],
    stopReason: "toolUse",
    usage: {
      input: 1,
      output: 1,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 2,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
  },
];

test("historical orphan tools restore without a spinner or an active run", options, (t) =>
  withApp(
    t,
    async (app) => {
      await app.openProject("alpha");
      await app.idle();
      assert.equal(await app.browser.$('aside.agent .tool[data-state="skipped"]').isExisting(), true);
      assert.equal(await app.browser.$("aside.agent .spin").isExisting(), false);
      assert.equal(app.model.requests.length, 0);
      await assert.rejects(access(join(app.data, "projects", "alpha", "alpha", "orphan.lua")));
    },
    { history: orphanHistory },
  ),
);

test(
  "agent tools use real project IPC and cannot traverse, use absolute paths or select another project",
  options,
  (t) =>
    withApp(t, async (app) => {
      await app.openProject("alpha");
      const tools = app.model.enqueue({
        tools: [
          {
            id: "allowed",
            name: "write_file",
            arguments: { path: "alpha/created.lua", content: "return 42\n", projectId: "beta", id: "beta" },
          },
          { id: "parent", name: "write_file", arguments: { path: "../beta/beta.lua", content: "MUST NOT WRITE" } },
          {
            id: "absolute",
            name: "write_file",
            arguments: { path: join(app.temporary, "outside.txt"), content: "MUST NOT WRITE" },
          },
          {
            id: "cross-project",
            name: "write_file",
            arguments: { path: "../beta/stolen.lua", content: "MUST NOT WRITE" },
          },
          { id: "read-parent", name: "read_file", arguments: { path: "../beta/beta.lua" } },
        ],
      });
      const final = app.model.enqueue({ text: "Native project tool checks completed." });
      await app.send("Run only the scripted project file operations");
      await received(tools);
      const followup = await received(final);
      await app.idle();
      const results = followup.messages.filter((message) => message.role === "tool");
      assert.equal(results.length, 5);
      assert.match(results.find((result) => result.tool_call_id === "allowed").content, /Wrote alpha\/created.lua/);
      for (const id of ["parent", "absolute", "cross-project", "read-parent"]) {
        assert.doesNotMatch(results.find((result) => result.tool_call_id === id).content, /Wrote /);
      }
      assert.equal((await app.browser.$$('aside.agent .tool[data-state="success"]')).length, 1);
      assert.equal((await app.browser.$$('aside.agent .tool[data-state="error"]')).length, 4);
      assert.equal(await readFile(join(app.data, "projects", "alpha", "alpha", "created.lua"), "utf8"), "return 42\n");
      assert.equal(
        await readFile(join(app.data, "projects", "beta", "beta.lua"), "utf8"),
        "-- E2E Beta untouched sentinel\nreturn 1\n",
      );
      assert.equal(await readFile(join(app.temporary, "outside.txt"), "utf8"), "outside project sentinel\n");
      await assert.rejects(access(join(app.data, "projects", "beta", "stolen.lua")));
      const direct = await app.invoke("project_read", { id: "alpha", path: "alpha/created.lua" });
      assert.deepEqual(direct, { ok: true, value: "return 42\n" });
      assert.equal((await app.invoke("project_read", { id: "alpha", path: "../beta/beta.lua" })).ok, false);
      assert.equal(
        (await app.invoke("project_write", { id: "../beta", path: "beta.lua", content: "MUST NOT WRITE" })).ok,
        false,
      );
    }),
);

test("leaving a project keeps its run going, saves progress as it goes and restores it on return", options, (t) =>
  withApp(t, async (app) => {
    await app.openProject("alpha");
    const held = app.model.enqueue({
      hold: true,
      thinking: "Alpha keeps working.",
      text: "Alpha finished in the background.",
    });
    await app.send("Keep working while I look at Beta");
    await received(held);
    await app.busy();
    await app.openProject("beta");
    await app.idle();
    await waitFor(
      async () => JSON.stringify(await app.chat("alpha")).includes("Keep working while I look at Beta"),
      "the running prompt to be saved before the run ends",
    );
    assert.notEqual(held.disconnected, true, "leaving the project must not cancel its request");
    assert.doesNotMatch(await app.browser.$("aside.agent").getText(), /Keep working while I look at Beta/);
    const beta = app.model.enqueue({ text: "Independent Beta response." });
    await app.send("Only answer in Beta");
    await received(beta);
    await app.idle();
    held.release();
    await waitFor(
      async () => JSON.stringify(await app.chat("alpha")).includes("Alpha finished in the background."),
      "the Alpha run to finish and save while Beta is open",
    );
    const betaHistory = JSON.stringify(await app.chat("beta"));
    assert.match(betaHistory, /Independent Beta response/);
    assert.doesNotMatch(betaHistory, /Keep working while I look at Beta|Alpha finished/);
    await app.openProject("alpha");
    await app.idle();
    const alphaText = await app.browser.$("aside.agent").getText();
    assert.match(alphaText, /Keep working while I look at Beta/);
    assert.match(alphaText, /Alpha finished in the background\./);
    assert.doesNotMatch(alphaText, /Independent Beta response/);
    await app.browser.refresh();
    await app.openProject("alpha");
    await app.idle();
    assert.match(await app.browser.$("aside.agent").getText(), /Alpha finished in the background\./);
  }),
);

test(
  "the assistant's project brief is saved, survives clearing the conversation and reaches the next request",
  options,
  (t) =>
    withApp(t, async (app) => {
      await app.openProject("alpha");
      await app.idle();
      const brief = "E2E brief: a breathing light that fades between 0 and 100.";
      const tools = app.model.enqueue({
        tools: [{ id: "brief", name: "update_project_brief", arguments: { brief } }],
      });
      const done = app.model.enqueue({ text: "Brief recorded." });
      await app.send("Remember what this project is");
      await received(tools);
      await received(done);
      await app.idle();
      await waitFor(async () => (await app.brief()) === brief, "the brief to be saved with the chat");
      await clearConversation(app);
      assert.equal(await app.brief(), brief, "clearing keeps the brief");
      const next = app.model.enqueue({ text: "Continuing from the brief." });
      await app.send("What were we building?");
      const body = await received(next);
      assert.match(JSON.stringify(body.messages), /E2E brief: a breathing light/);
      assert.doesNotMatch(JSON.stringify(body.messages), /Remember what this project is/);
      await app.idle();
      await app.browser.refresh();
      await app.openProject("alpha");
      await app.idle();
      await app.browser.$('button[aria-label="项目信息"]').click();
      const panel = await app.browser.$('aside[aria-label="项目信息"]');
      await panel.waitForDisplayed();
      assert.match(await panel.getText(), /E2E brief: a breathing light/);
    }),
);

async function setPermission(app, label, value) {
  await chooseSelect(app, label, value);
}

async function toolResults(app, tools) {
  const request = app.model.enqueue({ tools });
  const final = app.model.enqueue({ text: "Home tool checks done." });
  await app.send("Run the scripted home-folder operations");
  await received(request);
  const body = await received(final);
  await app.idle();
  return Object.fromEntries(
    body.messages.filter((m) => m.role === "tool").map((m) => [m.tool_call_id, String(m.content)]),
  );
}

test("home-folder access follows the project's setting and never reaches credentials", options, (t) =>
  withApp(t, async (app) => {
    await app.browser.setWindowSize(1000, 680);
    await app.openProject("alpha");
    await app.idle();
    // Default: read-only.
    let results = await toolResults(app, [
      { id: "list", name: "list_home", arguments: { path: "~/Documents" } },
      { id: "read", name: "read_home", arguments: { path: "~/Documents/notes.md" } },
      { id: "write", name: "write_home", arguments: { path: "~/Documents/new.md", content: "MUST NOT WRITE" } },
      { id: "secret", name: "read_home", arguments: { path: "~/.ssh/id_e2e" } },
      { id: "escape", name: "read_home", arguments: { path: "~/../app-data/chats/alpha.json" } },
    ]);
    assert.match(results.list, /notes\.md/);
    assert.match(results.read, /home notes sentinel/);
    assert.match(results.write, /只读/);
    assert.doesNotMatch(results.secret, /SECRET KEY/);
    assert.doesNotMatch(results.escape, /role/);
    await assert.rejects(access(join(app.home, "Documents", "new.md")));

    await app.browser.$('button[aria-label="项目信息"]').click();
    await app.browser.$(".workspace-info-permissions").waitForDisplayed();
    await setPermission(app, "项目外文件", "write");
    await waitFor(
      async () => (await app.invoke("agent_permissions_get", { id: "alpha" })).value?.files === "write",
      "the setting to be saved natively",
    );
    results = await toolResults(app, [
      { id: "write", name: "write_home", arguments: { path: "~/Documents/new.md", content: "written by e2e\n" } },
      { id: "dotfile", name: "write_home", arguments: { path: "~/.zshrc", content: "MUST NOT WRITE" } },
    ]);
    assert.match(results.write, /Wrote/);
    assert.equal(await readFile(join(app.home, "Documents", "new.md"), "utf8"), "written by e2e\n");
    assert.doesNotMatch(results.dotfile, /Wrote/);
    await assert.rejects(access(join(app.home, ".zshrc")));

    await setPermission(app, "项目外文件", "none");
    await setPermission(app, "推送到设备", "deny");
    await waitFor(
      async () => (await app.invoke("agent_permissions_get", { id: "alpha" })).value?.run === "deny",
      "run setting saved",
    );
    results = await toolResults(app, [
      { id: "read", name: "read_home", arguments: { path: "~/Documents/notes.md" } },
      { id: "push", name: "push_to_device", arguments: {} },
    ]);
    assert.doesNotMatch(results.read, /home notes sentinel/);
    assert.match(results.push, /disabled/);
    // Another project keeps its own default.
    assert.equal((await app.invoke("agent_permissions_get", { id: "beta" })).value?.files, "read");
  }),
);

test("retry replaces the last reply and rewind returns a prompt to the input", options, (t) =>
  withApp(t, async (app) => {
    await app.openProject("alpha");
    await app.idle();
    const first = app.model.enqueue({ text: "First draft of the answer." });
    await app.send("Answer this once");
    await received(first);
    await app.idle();
    const again = app.model.enqueue({ text: "Regenerated answer." });
    await app.browser.$('button[aria-label="重新生成"]').click();
    const body = await received(again);
    await app.idle();
    const prompts = body.messages.filter((m) => m.role === "user").map((m) => JSON.stringify(m.content));
    assert.equal(prompts.length, 1, "retry sends the prompt once");
    assert.match(prompts[0], /Answer this once/);
    assert.doesNotMatch(JSON.stringify(body.messages), /First draft of the answer/);
    await waitFor(
      async () => JSON.stringify(await app.chat()).includes("Regenerated answer."),
      "the retried reply to be saved",
    );
    assert.doesNotMatch(JSON.stringify(await app.chat()), /First draft of the answer/);

    await app.browser.execute(() => document.querySelector('button[aria-label="回退到这里"]').click());
    assert.equal(await app.browser.$("dialog[open]").isExisting(), false, "rewind confirms inline, not in a dialog");
    const confirm = await app.browser.$('button[aria-label="确认回退"]');
    await confirm.waitForDisplayed();
    await confirm.click();
    await waitFor(async () => (await app.chat()).length === 0, "the rewound conversation to be saved");
    assert.equal(await app.browser.$("aside.agent textarea").getValue(), "Answer this once");
    assert.equal(app.model.requests.length, 2, "rewinding sends nothing");
  }),
);
