// Bundle only curated bilingual guides and examples. Raw source/archive collections are not published.
import { createHash } from "node:crypto";
import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const docsDir = resolve(root, "docs/gizos");
const catalog = JSON.parse(await readFile(resolve(docsDir, "catalog.json"), "utf8"));
const localized = JSON.parse(await readFile(resolve(docsDir, "localized-guides.json"), "utf8"));
const titleOf = (text) => text.match(/^#\s+(.+)$/m)?.[1] ?? "GizOS";
async function document(file) {
  const path = resolve(docsDir, file);
  if (!path.startsWith(docsDir + sep)) throw new Error("Documentation path escapes docs/gizos");
  const text = await readFile(path, "utf8");
  return { file, text, sha256: createHash("sha256").update(text).digest("hex"), title: titleOf(text) };
}
const topics = [];
for (const metadata of catalog.topics) {
  if (!/^[a-z0-9-]+$/.test(metadata.id)) throw new Error("Invalid topic id");
  const example = ["input-example", "game-example"].includes(metadata.id);
  const file = metadata.file ?? `${metadata.id}.md`;
  const original = await document(file);
  const topic = {
    ...metadata,
    ...original,
    title: metadata.title,
    section: "lua",
    kind: example ? "example" : "guide",
    sourceLanguage: example ? "code" : "zh-CN",
  };
  if (!example) {
    const en = await document(`en/${metadata.id}.md`);
    topic.translations = {
      "zh-CN": { ...original, title: metadata.title, description: metadata.description },
      en: { ...en, description: metadata.description },
    };
  }
  topics.push(topic);
}
for (const metadata of localized) {
  const en = await document(`en/${metadata.id}.md`);
  const zh = await document(`zh-CN/${metadata.id}.md`);
  topics.push({
    ...metadata,
    ...en,
    kind: "guide",
    derived: true,
    sourceLanguage: "en",
    sourceLine: 1,
    revision: catalog.source.revision,
    translations: {
      en: { ...en, description: metadata.description },
      "zh-CN": { ...zh, description: metadata.description },
    },
  });
}
if (new Set(topics.map((topic) => topic.id)).size !== topics.length)
  throw new Error("Duplicate documentation topic ids");
if (topics.some((topic) => !["guide", "example"].includes(topic.kind) || topic.id.startsWith("ref-")))
  throw new Error("Raw reference content is not allowed in the documentation bundle");
const installed = resolve(root, "public/gizos-docs");
// This is a gitignored generated asset directory. Clear stale files from older bundles too.
await rm(installed, { recursive: true, force: true });
await mkdir(installed, { recursive: true });
await cp(docsDir, installed, { recursive: true });
await writeFile(resolve(root, "src/gizos-docs/snapshot.json"), JSON.stringify({ ...catalog, topics }, null, 2) + "\n");
console.log(
  `Bundled ${topics.length} curated GizOS topics: ${topics.filter((t) => t.kind === "guide").length} bilingual guides and ${topics.filter((t) => t.kind === "example").length} examples.`,
);
