import { randomUUID } from "node:crypto";
import type { Plugin } from "vite";
import { buildLuaPackage } from "../src/lua-package.ts";
import { parseProject } from "../src/project.ts";

const prefix = "/__gizmoth-preview/packages";
const ttl = 10 * 60 * 1000;
const maxStoredBytes = 16 * 1024 * 1024;

/** Development-only package delivery to the local Web firmware. No files or code are executed. */
export function previewPackages(): Plugin {
  const packages = new Map<string, { bytes: Uint8Array; expires: number }>();
  const prune = () => {
    for (const [key, value] of packages) if (value.expires < Date.now()) packages.delete(key);
  };
  return {
    name: "gizmoth-preview-packages",
    apply: "serve",
    configureServer(server) {
      server.httpServer?.once("close", () => packages.clear());
      server.middlewares.use(async (request, response, next) => {
        const path = request.url?.split("?")[0] ?? "";
        if (path !== prefix && !path.startsWith(prefix + "/")) return next();
        response.setHeader("Cache-Control", "no-store");
        response.setHeader("X-Content-Type-Options", "nosniff");
        const json = (status: number, value: unknown) => {
          response.statusCode = status;
          response.setHeader("Content-Type", "application/json; charset=utf-8");
          response.end(JSON.stringify(value));
        };
        prune();
        if (request.method === "GET") {
          const item = packages.get(path);
          if (!item) return json(404, { error: "预览程序包已过期，请重新运行当前项目。" });
          response.setHeader("Content-Type", "application/octet-stream");
          response.setHeader("Content-Length", item.bytes.length);
          response.end(item.bytes);
          return;
        }
        const origin = request.headers.origin;
        if (origin !== `http://${request.headers.host}` && origin !== `https://${request.headers.host}`)
          return json(403, { error: "仅允许本机预览页面创建程序包。" });
        if (request.method === "DELETE") {
          packages.delete(path);
          response.statusCode = 204;
          response.end();
          return;
        }
        if (request.method !== "POST" || path !== prefix) return json(405, { error: "不支持的预览请求。" });
        if (!request.headers["content-type"]?.startsWith("application/json"))
          return json(415, { error: "预览请求需要 JSON 项目文件。" });
        try {
          const chunks: Buffer[] = [];
          let size = 0;
          let oversized = false;
          for await (const chunk of request) {
            size += chunk.length;
            if (size > 4 * 1024 * 1024) {
              oversized = true;
              continue;
            }
            chunks.push(Buffer.from(chunk));
          }
          if (oversized) return json(413, { error: "预览项目请求超过 4 MiB。" });
          const source = parseProject(JSON.parse(Buffer.concat(chunks).toString("utf8")));
          const { bytes, sha256, config } = await buildLuaPackage(source);
          let total = [...packages.values()].reduce((sum, item) => sum + item.bytes.length, bytes.length);
          for (const [key, item] of packages) {
            if (total <= maxStoredBytes && packages.size < 32) break;
            packages.delete(key);
            total -= item.bytes.length;
          }
          const url = `${prefix}/${randomUUID()}.lua-app.tar.zlib`;
          packages.set(url, { bytes, expires: Date.now() + ttl });
          json(201, {
            path: url,
            sha256,
            appId: config.app_id,
            version: config.version,
            name: config.display_name ?? config.app_id,
          });
        } catch (error) {
          json(400, { error: error instanceof Error ? error.message : "无法生成当前项目的预览包。" });
        }
      });
    },
  };
}
