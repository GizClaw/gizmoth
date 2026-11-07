import { inspectProject, type Project } from "./project.ts";

const encoder = new TextEncoder();
const hash = async (bytes: Uint8Array<ArrayBuffer>) =>
  [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map((value) => value.toString(16).padStart(2, "0"))
    .join("");

function tarEntry(path: string, data: Uint8Array<ArrayBuffer>): Uint8Array<ArrayBuffer> {
  const header = new Uint8Array(512);
  let name = path;
  let prefix = "";
  if (encoder.encode(name).length > 100) {
    let split = path.lastIndexOf("/");
    while (split > 0) {
      const left = path.slice(0, split);
      const right = path.slice(split + 1);
      if (encoder.encode(left).length <= 155 && encoder.encode(right).length <= 100) {
        prefix = left;
        name = right;
        break;
      }
      split = path.lastIndexOf("/", split - 1);
    }
    if (!prefix) throw new Error(`文件路径无法编码为 USTAR，请缩短文件名：${path}`);
  }
  const text = (offset: number, value: string) => header.set(encoder.encode(value), offset);
  const octal = (offset: number, width: number, value: number) =>
    text(offset, value.toString(8).padStart(width - 1, "0") + "\0");
  text(0, name);
  octal(100, 8, 0o644);
  octal(108, 8, 0);
  octal(116, 8, 0);
  octal(124, 12, data.length);
  octal(136, 12, 0);
  header.fill(32, 148, 156);
  text(156, "0");
  text(257, "ustar\0");
  text(263, "00");
  text(345, prefix);
  text(
    148,
    header
      .reduce((total, value) => total + value, 0)
      .toString(8)
      .padStart(6, "0") + "\0 ",
  );
  const block = new Uint8Array(512 + Math.ceil(data.length / 512) * 512);
  block.set(header);
  block.set(data, 512);
  return block;
}

/** Format-1 Lua package, matching the manifest/USTAR contract in project.rs. */
export async function buildLuaPackage(project: Project) {
  const inspected = inspectProject(project);
  if (!inspected.config || inspected.errors.length) throw new Error(inspected.errors.join("\n"));
  const config = inspected.config;
  const payload = project.files
    .filter((file) => file.path !== "app.json")
    .sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  const sources = payload.map((file) => ({ path: file.path, data: encoder.encode(file.content) }));
  const files = await Promise.all(
    sources.map(async (file) => ({ path: file.path, size: file.data.length, sha256: await hash(file.data) })),
  );
  const manifest = encoder.encode(
    JSON.stringify({
      format: 1,
      type: "lua-app",
      app_id: config.app_id,
      version: config.version,
      ...(config.display_name !== undefined ? { display_name: config.display_name } : {}),
      ...(config.description !== undefined ? { description: config.description } : {}),
      entry: `${config.app_id}.lua`,
      data_dir: payload.length > 1 ? config.app_id : null,
      compact: false,
      files,
    }),
  );
  if (manifest.length > 32768) throw new Error("manifest 超过 32 KiB。");
  const entries = [tarEntry("manifest.json", manifest), ...sources.map((file) => tarEntry(file.path, file.data))];
  const tar = new Uint8Array(entries.reduce((total, entry) => total + entry.length, 1024));
  let offset = 0;
  for (const entry of entries) {
    tar.set(entry, offset);
    offset += entry.length;
  }
  const compressed = new Blob([tar.buffer]).stream().pipeThrough(new CompressionStream("deflate"));
  const bytes = new Uint8Array(await new Response(compressed).arrayBuffer());
  return { config, bytes, files: payload.length, sha256: await hash(bytes) };
}
