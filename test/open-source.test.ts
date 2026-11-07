import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { gizmothRepository, openSourceProjects, projectRepository } from "../src/open-source.ts";

// Keep the desktop URL capability in sync with the curated links; no wildcard opener.
test("public project links and the Star target match the desktop opener scope", () => {
  const capability = JSON.parse(
    readFileSync(new URL("../src-tauri/capabilities/default.json", import.meta.url), "utf8"),
  );
  const scope = capability.permissions.find((p: { identifier?: string }) => p.identifier === "opener:allow-open-url");
  const urls = openSourceProjects.map((project) => projectRepository(project.repository));
  assert.equal(new Set(urls).size, openSourceProjects.length);
  assert.deepEqual(
    openSourceProjects.map((project) => project.name),
    ["GizOS", "GizClaw", "GizMoth"],
  );
  const ownProject = openSourceProjects.find((project) => project.repository === "gizmoth");
  assert.ok(ownProject);
  assert.equal(projectRepository(ownProject.repository), gizmothRepository);
  assert.deepEqual(scope.allow.map((p: { url: string }) => p.url).sort(), [...urls].sort());
  for (const project of openSourceProjects) {
    assert.ok(project.name && project.description);
    const url = new URL(projectRepository(project.repository));
    assert.equal(url.protocol, "https:");
    assert.equal(url.hostname, "github.com");
    assert.match(url.pathname, /^\/GizClaw\/[a-z0-9-]+$/);
  }
});
