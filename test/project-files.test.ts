import assert from "node:assert/strict";
import test from "node:test";
import { projectFileTree, projectPathSegments, projectTreeChildren, visibleProjectTree } from "../src/project-files.ts";

const tree = projectFileTree([
  { path: "app.json" },
  { path: "game/main.lua" },
  { path: "game/second.lua" },
  { path: "game/assets/sprite.json" },
  { path: "other/main.lua" },
]);

test("file breadcrumbs scope each segment to its own siblings", () => {
  assert.deepEqual(projectPathSegments(""), []);
  assert.deepEqual(projectPathSegments("app.json"), [{ name: "app.json", path: "app.json", parent: "", file: true }]);
  const segments = projectPathSegments("game/assets/sprite.json");
  assert.deepEqual(segments, [
    { name: "game", path: "game", parent: "", file: false },
    { name: "assets", path: "game/assets", parent: "game", file: false },
    { name: "sprite.json", path: "game/assets/sprite.json", parent: "game/assets", file: true },
  ]);
  assert.deepEqual(
    projectTreeChildren(tree, segments[0].parent).map((node) => node.path),
    ["game", "other", "app.json"],
  );
  assert.deepEqual(
    projectTreeChildren(tree, segments[1].parent).map((node) => node.path),
    ["game/assets", "game/main.lua", "game/second.lua"],
  );
  assert.deepEqual(
    projectTreeChildren(tree, segments[2].parent).map((node) => node.path),
    ["game/assets/sprite.json"],
  );
  for (const missing of ["missing", "game/missing", "app.json", "game/main.lua"])
    assert.deepEqual(projectTreeChildren(tree, missing), []);
});

test("breadcrumb tree expansion exposes only descendants of expanded folders", () => {
  const visible = (folders: string[]) =>
    visibleProjectTree(tree, new Set(folders)).map(({ node, depth }) => [node.path, depth]);
  assert.deepEqual(visible(["game/assets"]), [
    ["game", 0],
    ["other", 0],
    ["app.json", 0],
  ]);
  assert.deepEqual(visible(["game"]), [
    ["game", 0],
    ["game/assets", 1],
    ["game/main.lua", 1],
    ["game/second.lua", 1],
    ["other", 0],
    ["app.json", 0],
  ]);
  assert.deepEqual(visible(["game", "game/assets"]), [
    ["game", 0],
    ["game/assets", 1],
    ["game/assets/sprite.json", 2],
    ["game/main.lua", 1],
    ["game/second.lua", 1],
    ["other", 0],
    ["app.json", 0],
  ]);
  assert.equal(projectTreeChildren(tree, "game")[0].path, "game/assets", "expansion does not mutate the project tree");
});
