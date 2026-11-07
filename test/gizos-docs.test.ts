import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { builtinGizosMcp, gizosDocumentationTools } from "../src/gizos-docs/index.ts";
import { bundledGizosDocs } from "../src/gizos-docs/data.ts";
import { connectGizosDocs } from "../src/gizos-docs/client.ts";
import { searchTopics, type DocumentationSnapshot } from "../src/gizos-docs/catalog.ts";

const fixture: DocumentationSnapshot = {
  format: 1,
  source: { repository: "https://github.com/GizClaw/gizos", revision: "0123456789abcdef" },
  topics: [
    {
      id: "graphics",
      title: "Graphics reference",
      description: "Drawing and display",
      keywords: ["screen", "graphics", "draw"],
      path: "reference/graphics.md",
      sourceLine: 10,
      sha256: "fixture",
      text: "# Graphics\n\nDrawing reference.\nLine four.\nLine five.",
    },
    {
      id: "input",
      title: "Buttons reference",
      description: "Physical input and key events",
      keywords: ["button", "input", "key"],
      path: "reference/input.md",
      sourceLine: 1,
      sha256: "fixture",
      text: "# Input\n\nButtons reference.",
    },
  ],
};

test("real MCP initialization exposes documentation tools and resources", async () => {
  const connection = await connectGizosDocs(fixture);
  try {
    assert.equal(connection.client.getServerVersion()?.name, "gizos-docs");
    assert.deepEqual(
      connection.tools.map((tool) => tool.name),
      ["gizos_docs_search", "gizos_docs_read"],
    );
    const { resources } = await connection.client.listResources();
    assert.deepEqual(
      resources.map((resource) => resource.uri),
      ["gizos://docs/graphics", "gizos://docs/input"],
    );
    const resource = await connection.client.readResource({ uri: resources[0].uri });
    assert.ok("text" in resource.contents[0]);
    assert.equal(resource.contents[0].text, fixture.topics[0].text);
    await assert.rejects(connection.client.readResource({ uri: "file:///etc/passwd" }), /Unknown.*resource/);
  } finally {
    await connection.close();
  }
});

test("pi-agent tools read through MCP with pagination and pinned source citations", async () => {
  const connection = await connectGizosDocs(fixture);
  try {
    const search = await connection.tools[0].execute("search", { query: "绘图", limit: 1 });
    const text = JSON.stringify(search.content);
    assert.match(text, /gizos:\/\/docs\/(?:en\/)?graphics/);
    assert.match(text, /0123456789abcdef/);
    assert.doesNotMatch(text, /gizos:\/\/docs\/(?:en\/)?input/);
    const read = await connection.tools[1].execute("read", { topic: "graphics", startLine: 2, maxLines: 2 });
    const page = JSON.stringify(read.content);
    assert.match(page, /Topic lines 2–3 of 5/);
    assert.match(page, /graphics\.md#L11/);
    assert.match(page, /startLine=4/);
    const index = await connection.tools[0].execute("index", { query: "" });
    assert.match(JSON.stringify(index.content), /Buttons reference/);
  } finally {
    await connection.close();
  }
});

test("malformed tool arguments and arbitrary paths cannot reach a reader", async () => {
  const connection = await connectGizosDocs(fixture);
  try {
    await assert.rejects(connection.tools[1].execute("path", { topic: "../../secrets" }), /Unknown topic/);
    await assert.rejects(connection.tools[1].execute("range", { topic: "graphics", startLine: 999 }), /out of range/);
    for (const args of [{}, { query: 2 }, { query: "x", limit: 999 }, { query: "x", injected: true }])
      await assert.rejects(connection.tools[0].execute("invalid", args), /MCP error/);
    const controller = new AbortController();
    controller.abort();
    await assert.rejects(connection.tools[0].execute("cancelled", { query: "drawing" }, controller.signal), {
      name: "AbortError",
    });
  } finally {
    await connection.close();
  }
});

test("Chinese button queries find input docs and misses stay explicit", () => {
  assert.equal(searchTopics(fixture, "按键", 1)[0].topic.id, "input");
  assert.deepEqual(searchTopics(fixture, "nonexistentfunctionxyz", 3), []);
});

test("the installed MCP snapshot contains the exact committed Markdown and its hashes", async () => {
  for (const topic of bundledGizosDocs.topics) {
    const markdown = await readFile(
      new URL(`../docs/gizos/${topic.file ?? `${topic.id}.md`}`, import.meta.url),
      "utf8",
    );
    assert.equal(topic.text, markdown, `${topic.id} must share the reader's Markdown`);
    assert.equal(topic.sha256, createHash("sha256").update(markdown).digest("hex"));
    assert.match(topic.revision ?? "", /^[a-f0-9]{40}$/);
    for (const translation of Object.values(topic.translations ?? {})) {
      const localized = await readFile(new URL(`../docs/gizos/${translation.file}`, import.meta.url), "utf8");
      assert.equal(localized, translation.text);
      assert.equal(translation.sha256, createHash("sha256").update(localized).digest("hex"));
    }
  }
  const example = bundledGizosDocs.topics.find((topic) => topic.id === "input-example")!;
  assert.match(example.text, /require\("display"\)/);
  assert.match(example.text, /runtime\.components\.on/);
  assert.match(example.text, /display\.present\(\)/);
  assert.match(example.text, /runtime\.sleep\(50\)/);
});

test("the default docs MCP is initialized once and provides the official device example", async () => {
  assert.equal(builtinGizosMcp.enabled, true);
  assert.equal(builtinGizosMcp.transport, "in-memory");
  const [first, second] = await Promise.all([gizosDocumentationTools(), gizosDocumentationTools()]);
  assert.equal(first, second);
  const read = first.find((tool) => tool.name === "gizos_docs_read")!;
  const response = await read.execute("read", { topic: "input-example" });
  assert.match(JSON.stringify(response.content), /display\.draw_text/);
});

test("the same documentation server can be used by a standard stdio MCP client", async () => {
  const client = new Client({ name: "gizmoth-docs-test", version: "1.0.0" });
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: ["--experimental-strip-types", fileURLToPath(new URL("../scripts/gizos-docs-mcp.mjs", import.meta.url))],
    stderr: "pipe",
  });
  try {
    await client.connect(transport);
    const { tools } = await client.listTools();
    assert.equal(tools.length, 2);
    const result = await client.callTool({ name: "gizos_docs_read", arguments: { topic: "input-example" } });
    assert.match(JSON.stringify(result), /runtime\.sleep/);
  } finally {
    await client.close();
    await transport.close();
  }
});

test("the corpus covers the platform and MCP reads the same topic in Chinese and English", async () => {
  const { findTopic, documentationSections } = await import("../src/gizos-docs/catalog.ts");
  for (const section of documentationSections)
    assert.ok(
      bundledGizosDocs.topics.some((topic) => topic.section === section.id && topic.translations),
      section.id,
    );
  assert.ok(bundledGizosDocs.topics.every((topic) => topic.kind === "guide" || topic.kind === "example"));
  assert.ok(bundledGizosDocs.topics.every((topic) => !topic.id.startsWith("ref-")));
  assert.equal(
    bundledGizosDocs.topics.find((topic) => topic.id === "display-bindings"),
    undefined,
  );
  const connection = await connectGizosDocs(bundledGizosDocs);
  try {
    const read = connection.tools.find((tool) => tool.name === "gizos_docs_read")!;
    const en = await read.execute("en", { topic: "gizos-overview", language: "en" });
    const zh = await read.execute("zh", { topic: "gizos-overview", language: "zh-CN" });
    assert.match(JSON.stringify(en.content), /portable firmware and application platform/);
    assert.match(JSON.stringify(zh.content), /可移植平台/);
    assert.equal(findTopic(bundledGizosDocs, "gizos://docs/en/gizos-overview")!.sourceLanguage, "en");
    assert.equal(findTopic(bundledGizosDocs, "gizos://lua/input-example")!.id, "input-example");
    const resources = await connection.client.listResources();
    assert.ok(resources.resources.some((item) => item.uri === "gizos://docs/en/lua-device-api"));
    assert.ok(resources.resources.every((item) => !item.uri.includes("ref-")));
  } finally {
    await connection.close();
  }
});

test("Lua translations preserve all literal code blocks", async () => {
  const blocks = (text: string) => [...text.matchAll(/```[^\n]*\n([\s\S]*?)```/g)].map((match) => match[1].trim());
  for (const topic of bundledGizosDocs.topics.filter((topic) => topic.section === "lua" && topic.translations?.en))
    assert.deepEqual(blocks(topic.text), blocks(topic.translations!.en!.text), topic.id);
});
