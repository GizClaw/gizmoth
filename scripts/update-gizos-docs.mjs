// Rebuild the offline MCP corpus from immutable Git objects, never working-tree files.
// Usage: node scripts/update-gizos-docs.mjs /path/to/GizClaw/gizos
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repo = process.argv[2];
if (!repo) throw new Error("Usage: node scripts/update-gizos-docs.mjs /path/to/GizClaw/gizos");
const root = fileURLToPath(new URL("../", import.meta.url));
const source = JSON.parse(await readFile(resolve(root, "src/gizos-docs/sources.json"), "utf8"));
const git = (...args) =>
  execFileSync("git", ["-C", resolve(repo), ...args], { encoding: "utf8", maxBuffer: 8 * 1024 * 1024 });
for (const revision of [source.revision, source.guideRevision]) {
  if (!/^[a-f0-9]{40}$/.test(revision)) throw new Error("Documentation revisions must be full Git commit ids");
  git("cat-file", "-e", `${revision}^{commit}`);
}
// The prose was removed from newer guides, but is valid only while its runtime bindings still match.
if (git("diff", "--name-only", source.guideRevision, source.revision, "--", "libs/lua").trim())
  throw new Error("Lua Runtime changed since the prose guide. Review/update the prose snapshot before bundling.");

const cache = new Map();
function pinnedFile(revision, path) {
  const key = `${revision}:${path}`;
  if (!cache.has(key)) cache.set(key, git("show", key));
  return cache.get(key);
}
const topics = [];
for (const entry of [
  ...source.guideTopics.map((topic) => ({ ...topic, path: source.guidePath, revision: source.guideRevision })),
  ...source.sourceTopics.map((topic) => ({ ...topic, revision: source.revision })),
]) {
  const file = pinnedFile(entry.revision, entry.path);
  const lines = file.split("\n");
  const start = entry.startLine ?? 1;
  const end = entry.endLine ?? lines.length;
  if (start < 1 || end < start || end > lines.length) throw new Error(`Invalid topic line range: ${entry.id}`);
  const text = lines.slice(start - 1, end).join("\n");
  topics.push({
    id: entry.id,
    title: entry.title,
    description: entry.description,
    keywords: entry.keywords,
    path: entry.path,
    sourceLine: start,
    revision: entry.revision,
    sha256: createHash("sha256").update(text).digest("hex"),
    text,
  });
}
if (new Set(topics.map((topic) => topic.id)).size !== topics.length) throw new Error("Topic ids must be unique");
// Markdown is the committed, human-readable source. Builds bundle these exact files into MCP.
const docsDir = resolve(root, "docs/gizos");
await mkdir(docsDir, { recursive: true });
const catalogTopics = [];
for (const topic of topics) {
  const language = topic.path.endsWith(".lua")
    ? "lua"
    : /\.[ch]$/.test(topic.path)
      ? "c"
      : topic.path.endsWith(".bzl")
        ? "python"
        : null;
  const prefix = `# ${topic.title}\n\n${topic.description}\n\n`;
  const footer = `\n\n## 来源\n\nSource: [${topic.path}](${source.repository}/blob/${topic.revision}/${topic.path}#L${topic.sourceLine}), revision \`${topic.revision}\`.\n`;
  const markdown = language
    ? `${prefix}\`\`\`${language}\n${topic.text}\n\`\`\`${footer}`
    : `${prefix}${topic.text.trimEnd()}${footer}`;
  const { text: _text, sha256: sourceSha256, ...metadata } = topic;
  catalogTopics.push({ ...metadata, sourceStart: prefix.split("\n").length + (language ? 1 : 0), sourceSha256 });
  await writeFile(resolve(docsDir, `${topic.id}.md`), markdown);
}
await writeFile(
  resolve(docsDir, "catalog.json"),
  `${JSON.stringify({ format: 1, source: { repository: source.repository, revision: source.revision }, topics: catalogTopics }, null, 2)}\n`,
);
// The platform-wide documentation index and localized guides are maintained separately.
await import("./bundle-gizos-docs.mjs");
// Ship upstream licenses as readable assets in browser and desktop builds too.
const licenseDir = resolve(root, "public/licenses");
await mkdir(licenseDir, { recursive: true });
for (const [id, filename] of [
  ["license", "gizos-LICENSE.txt"],
  ["game-license", "gizos-examples-LICENSE.txt"],
  ["game-provenance", "gizos-examples-SOURCE.md"],
]) {
  const license = source.licenseSources.find((entry) => entry.id === id);
  if (!license) throw new Error(`Missing upstream license: ${id}`);
  await writeFile(resolve(licenseDir, filename), pinnedFile(source.revision, license.path));
}
console.log(`Bundled ${topics.length} GizOS documentation topics from ${source.revision}.`);
