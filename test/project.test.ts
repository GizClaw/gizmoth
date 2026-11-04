import assert from "node:assert/strict";
import test from "node:test";
import { byteLength, inspectProject, parseProject, starterProject, validatePath } from "../src/project.ts";

test("starter project matches firmware entry naming and uses UTF-8 byte counts", () => {
  const p = starterProject();
  assert.deepEqual(inspectProject(p).errors, []);
  assert.equal(inspectProject(p).config?.app_id, "hello");
  assert.equal(byteLength("你好"), 6);
  assert.deepEqual(parseProject(JSON.parse(JSON.stringify(p))), p);
});
test("rejects traversal, hidden paths and case-insensitive duplicates", () => {
  for (const path of ["../file", "/hello", "hello//x", "hello/.env", "hello/x/", "hello\\x", "hello:x"])
    assert.ok(validatePath(path), path);
  const p = starterProject();
  p.files.push({ path: "HELLO.LUA", content: "" });
  assert.throws(() => parseProject(p), /重复/);
});
test("rejects file-directory collisions", () => {
  const p = starterProject();
  p.files.push({ path: "hello/data", content: "" }, { path: "hello/data/x", content: "" });
  assert.throws(() => parseProject(p), /冲突/);
});
test("changing app id requires renamed entry and correctly rooted assets", () => {
  const p = starterProject();
  p.files[0].content = '{"app_id":"other","version":"1.0.0"}';
  assert.match(inspectProject(p).errors[0], /other.lua/);
  p.files[1].path = "other.lua";
  p.files.push({ path: "hello/data.txt", content: "example" });
  assert.match(inspectProject(p).errors[0], /other\//);
  p.files[2].path = "other/data.txt";
  assert.deepEqual(inspectProject(p).errors, []);
});
test("rejects entry bytecode, invalid SemVer, oversized UTF-8 names", () => {
  let p = starterProject();
  p.files[1].content = "\x1bLua";
  assert.ok(inspectProject(p).errors.length);
  for (const version of ["1.0", "01.0.0", "1.0.0-01", "v1.0.0"]) {
    p = starterProject();
    p.files[0].content = JSON.stringify({ app_id: "hello", version });
    assert.ok(inspectProject(p).errors.length, version);
  }
  p = starterProject();
  p.files[0].content = JSON.stringify({
    app_id: "hello",
    version: "1.0.0",
    display_name: "你".repeat(43),
  });
  assert.ok(inspectProject(p).errors.length);
});
test("rejects malformed and oversized imported workspace without changing source", () => {
  assert.throws(() => parseProject({ version: 1, files: [] }));
  const p = starterProject();
  p.files[1].content = "x".repeat(3 * 1024 * 1024);
  assert.throws(() => parseProject(p), /2 MiB/);
});
