import test from "node:test";
import assert from "node:assert/strict";
import { detectEditorLanguage } from "../src/editor-language.ts";
import { formatEditorCode } from "../src/editor-format.ts";
import { projectFileTree } from "../src/project-files.ts";

test("editor language detection leaves unknown files as plain text", () => {
  for (const [path, language] of [
    ["main.lua", "lua"],
    ["app.JSON", "json"],
    ["script.ts", "typescript"],
    ["ui.jsx", "javascript"],
    ["page.html", "html"],
    ["style.css", "css"],
    ["README.md", "markdown"],
    ["config.yml", "yaml"],
    ["assets/123", "text"],
  ])
    assert.equal(detectEditorLanguage(path), language);
});
test("formatting JSON preserves values and rejects invalid source", async () => {
  const source = '{"name":"空 格", "items":[1,true,null]}';
  const formatted = await formatEditorCode(source, "json");
  assert.deepEqual(JSON.parse(formatted), JSON.parse(source));
  assert.ok(formatted.includes("\n"));
  assert.equal(await formatEditorCode(formatted, "json"), formatted);
  await assert.rejects(formatEditorCode('{"name":', "json"));
  await assert.rejects(formatEditorCode(source, "text"));
});
test("formats supported web languages with bundled parsers", async () => {
  for (const [language, source] of [
    ["javascript", "const x={a:1};console.log(x)"],
    ["typescript", "const x:number=1"],
    ["html", "<main><p>Hello</p></main>"],
    ["css", ".a{color:red;}"],
    ["markdown", "#  Title\n\n- one\n- two"],
    ["yaml", "name:  hello\nitems: [1,2]"],
  ] as const) {
    const formatted = await formatEditorCode(source, language);
    assert.equal(await formatEditorCode(formatted, language), formatted);
    assert.ok(formatted.endsWith("\n"));
  }
});
test("file tree retains directories and duplicate basenames without change metadata", () => {
  const tree = projectFileTree([{ path: "app.json" }, { path: "one/main.lua" }, { path: "two/main.lua" }]);
  assert.deepEqual(
    tree.map((node) => node.name),
    ["one", "two", "app.json"],
  );
  assert.equal(tree[0].children?.[0].path, "one/main.lua");
  assert.equal(tree[1].children?.[0].path, "two/main.lua");
});
