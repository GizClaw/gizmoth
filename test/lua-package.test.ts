import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { inflateSync } from "node:zlib";
import { buildLuaPackage } from "../src/lua-package.ts";

const project = {
  version: 1 as const,
  files: [
    {
      path: "app.json",
      content: JSON.stringify({ app_id: "game", version: "1.2.3", display_name: "当前游戏", format: 99, type: "bad" }),
    },
    { path: "game.lua", content: "print('当前版本')\n" },
    { path: `game/${"directory/".repeat(12)}asset.txt`, content: "resource" },
  ],
};

test("preview package is a deterministic zlib USTAR with current bytes and integrity hashes", async () => {
  const first = await buildLuaPackage(project);
  const second = await buildLuaPackage(project);
  assert.deepEqual(first.bytes, second.bytes);
  assert.equal(first.sha256, createHash("sha256").update(first.bytes).digest("hex"));
  const tar = inflateSync(first.bytes);
  const entries = new Map<string, Buffer>();
  for (let offset = 0; tar[offset];) {
    const header = tar.subarray(offset, offset + 512);
    const value = (start: number, size: number) =>
      header
        .subarray(start, start + size)
        .toString("utf8")
        .replace(/\0.*$/s, "");
    const size = parseInt(value(124, 12), 8);
    assert.equal(value(257, 6), "ustar");
    const sum = [...header].reduce((total, byte, index) => total + (index >= 148 && index < 156 ? 32 : byte), 0);
    assert.equal(parseInt(value(148, 8), 8), sum);
    const prefix = value(345, 155);
    entries.set(`${prefix ? prefix + "/" : ""}${value(0, 100)}`, tar.subarray(offset + 512, offset + 512 + size));
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  assert.equal([...entries.keys()][0], "manifest.json");
  assert(!entries.has("app.json"));
  const manifest = JSON.parse(entries.get("manifest.json")!.toString());
  assert.equal(manifest.format, 1);
  assert.equal(manifest.type, "lua-app");
  assert.equal(manifest.app_id, "game");
  for (const file of manifest.files) {
    const bytes = entries.get(file.path)!;
    assert.equal(bytes.length, file.size);
    assert.equal(createHash("sha256").update(bytes).digest("hex"), file.sha256);
  }
  assert.equal(entries.get("game.lua")!.toString(), project.files[1].content);
});

test("edited source changes the actual package; invalid projects cannot be installed", async () => {
  const first = await buildLuaPackage(project);
  const edited = {
    ...project,
    files: project.files.map((file) => (file.path === "game.lua" ? { ...file, content: "print('edited')" } : file)),
  };
  assert.notEqual((await buildLuaPackage(edited)).sha256, first.sha256);
  await assert.rejects(
    buildLuaPackage({ ...project, files: project.files.filter((file) => file.path !== "game.lua") }),
    /缺少入口/,
  );
});
