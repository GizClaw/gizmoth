import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { readFile, stat } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { waitFor, withApp } from "./native-app.mjs";

const exec = promisify(execFile);

// Opt-in: real public Git transport, isolated app data, no tool installation.
test(
  "native development catalog clones once, pulls, and survives offline restart",
  { skip: process.env.GIZMOTH_LIVE_DEVENV_TEST !== "1", timeout: 240000 },
  (t) =>
    withApp(t, async (app) => {
      await app.browser.$('button[data-page="chips"]').click();
      await waitFor(async () => await app.browser.$(".devenv-registry").isExisting(), "first registry clone", 150000);
      const refresh = () => app.browser.$('button[aria-label="刷新开发环境目录"]');
      await refresh().waitForEnabled();
      const initial = await app.invoke("devenv_state");
      assert.equal(initial.ok, true);
      assert.ok(initial.value.chips.length > 0);
      assert.equal(initial.value.registry.repository, "https://github.com/GizClaw/gizmoth-devenv");
      const repository = join(app.data, "devenv", "registry", "repository");
      assert.equal((await stat(join(repository, ".git"))).isDirectory(), true);
      const git = (...args) => exec("git", ["-C", repository, ...args], { timeout: 10000 });
      const head = (await git("rev-parse", "HEAD")).stdout.trim();
      assert.equal(head, initial.value.registry.commit);
      await git("config", "gizmoth.test", "persistent-clone");
      await refresh().click();
      let updated;
      await waitFor(
        async () => {
          updated = await app.invoke("devenv_state");
          return updated.ok && updated.value.registry?.fetchedAt > initial.value.registry.fetchedAt;
        },
        "registry pull publication",
        150000,
      );
      await refresh().waitForEnabled();
      assert.equal((await git("config", "gizmoth.test")).stdout.trim(), "persistent-clone");
      assert.equal(updated.value.registry.commit, (await git("rev-parse", "HEAD")).stdout.trim());
      await app.screenshot("devenv-git-catalog");

      // Break only this checkout's HTTP transport, leaving the rest of the app online.
      await git("config", "remote.origin.proxy", "http://127.0.0.1:1");
      await refresh().click();
      await app.browser.$(".devenv-error").waitForDisplayed({ timeout: 30000 });
      assert.match(await app.browser.$(".devenv-error").getText(), /Git/);
      assert.equal((await app.invoke("devenv_state")).value.registry.commit, updated.value.registry.commit);
      const pointer = JSON.parse(await readFile(join(app.data, "devenv/registry/current.json"), "utf8"));
      assert.equal(pointer.commit, updated.value.registry.commit);
      assert.equal((await app.browser.$$(".devenv-chip")).length, initial.value.chips.length);
      await app.screenshot("devenv-git-offline-refresh");

      await app.restart();
      await app.browser.$('button[data-page="chips"]').click();
      await app.browser.$(".devenv-registry").waitForDisplayed();
      const cached = await app.invoke("devenv_state");
      assert.equal(cached.ok, true);
      assert.deepEqual(cached.value.registry, updated.value.registry);
      assert.equal(cached.value.chips.length, initial.value.chips.length);
      assert.equal(await app.browser.$(".devenv-error").isExisting(), false);
      await app.screenshot("devenv-git-offline-restart");
    }),
);
