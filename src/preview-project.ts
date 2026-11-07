import { invoke, isTauri } from "@tauri-apps/api/core";
import { inspectProject, type Project } from "./project.ts";
import { buildLuaPackage } from "./lua-package.ts";

export interface PreviewPackage {
  url: string;
  sha256: string;
  appId: string;
  version: string;
  name: string;
  dispose: () => Promise<void>;
}

/** Deliver a bounded temporary package through the native runtime or development server. */
export async function preparePreviewProject(project: Project): Promise<PreviewPackage> {
  const inspected = inspectProject(project);
  if (!inspected.config || inspected.errors.length) throw new Error(inspected.errors.join("\n"));
  if (isTauri()) {
    const pkg = await buildLuaPackage(project);
    const location = await invoke<{ id: string; url: string }>("simulator_package_put", {
      bytes: Array.from(pkg.bytes),
    });
    return {
      url: location.url,
      sha256: pkg.sha256,
      appId: pkg.config.app_id,
      version: pkg.config.version,
      name: pkg.config.display_name ?? pkg.config.app_id,
      dispose: () => invoke<void>("simulator_package_remove", { id: location.id }),
    };
  }
  const response = await fetch("/__gizmoth-preview/packages", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(project),
  });
  if (!response.headers.get("content-type")?.includes("application/json"))
    throw new Error("此运行环境尚未接入当前项目的预览打包服务。");
  const result = await response.json();
  if (!response.ok) throw new Error(typeof result.error === "string" ? result.error : "无法准备预览程序包。");
  if (
    typeof result.path !== "string" ||
    !/^\/__gizmoth-preview\/packages\/[0-9a-f-]+\.lua-app\.tar\.zlib$/.test(result.path) ||
    typeof result.sha256 !== "string" ||
    !/^[0-9a-f]{64}$/.test(result.sha256) ||
    result.appId !== inspected.config.app_id ||
    result.version !== inspected.config.version
  )
    throw new Error("当前项目的预览程序包信息不匹配。");
  const url = new URL(result.path, location.origin).href;
  return {
    url,
    sha256: result.sha256,
    appId: result.appId,
    version: result.version,
    name: inspected.config.display_name ?? result.appId,
    dispose: async () => {
      await fetch(url, { method: "DELETE" });
    },
  };
}
