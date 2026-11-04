export interface SourceFile {
  path: string;
  content: string;
}
export interface Project {
  version: 1;
  files: SourceFile[];
}
export interface AppConfig {
  app_id: string;
  version: string;
  display_name?: string;
  description?: string;
}
const encoder = new TextEncoder();
export const byteLength = (text: string) => encoder.encode(text).length;
export const validAppId = (id: string) => /^[a-z0-9_-][a-z0-9_.-]{0,31}$/.test(id);
const semver =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

export function starterProject(): Project {
  return {
    version: 1,
    files: [
      {
        path: "app.json",
        content:
          JSON.stringify(
            {
              app_id: "hello",
              version: "1.0.0",
              display_name: "Hello device",
              description: "我的第一个 Lua 程序",
            },
            null,
            2,
          ) + "\n",
      },
      {
        path: "hello.lua",
        content:
          '-- Hello, device!\n-- 从一个简单的程序开始。\n-- 安装后，点击「运行」在设备上执行。\n\nlocal message = "Hello from LiteLink"\nprint(message)\n\nreturn { message = message }\n',
      },
    ],
  };
}

export function validatePath(path: string): string | undefined {
  if (
    !path ||
    byteLength(path) >= 192 ||
    /[\\:\x00-\x1f\x7f]/.test(path) ||
    path.split("/").some((part) => !part || part.startsWith("."))
  )
    return "文件名必须是相对路径，不能包含隐藏目录、空路径或特殊控制字符。";
}

export function parseProject(value: unknown): Project {
  if (
    !value ||
    typeof value !== "object" ||
    !("version" in value) ||
    value.version !== 1 ||
    !("files" in value) ||
    !Array.isArray(value.files) ||
    value.files.length < 2 ||
    value.files.length > 65
  )
    throw new Error("不是有效的 LiteLink 工作区（需要 2–65 个文件）。");
  let total = 0;
  const files: SourceFile[] = value.files.map((file: unknown) => {
    if (
      !file ||
      typeof file !== "object" ||
      !("path" in file) ||
      typeof file.path !== "string" ||
      !("content" in file) ||
      typeof file.content !== "string"
    )
      throw new Error("工作区文件格式无效。");
    const error = validatePath(file.path);
    if (error) throw new Error(error);
    total += byteLength(file.content);
    return { path: file.path, content: file.content };
  });
  if (total > 2 * 1024 * 1024 + 32768) throw new Error("工作区超过 2 MiB 限制。");
  const paths = files.map((f) => f.path.toLowerCase());
  if (
    new Set(paths).size !== paths.length ||
    paths.some((path) => paths.some((other) => path !== other && other.startsWith(path + "/")))
  )
    throw new Error("文件路径重复，或文件与目录冲突。");
  if (!files.some((f) => f.path === "app.json")) throw new Error("工作区缺少 app.json。");
  return { version: 1, files };
}

export function inspectProject(project: Project): {
  config?: AppConfig;
  errors: string[];
  bytes: number;
} {
  let bytes = 0;
  try {
    parseProject(project);
    const config: unknown = JSON.parse(project.files.find((f) => f.path === "app.json")!.content);
    if (
      !config ||
      typeof config !== "object" ||
      !("app_id" in config) ||
      typeof config.app_id !== "string" ||
      !validAppId(config.app_id)
    )
      throw new Error("app_id 需为 1–32 位小写字母、数字、下划线、点或连字符，不能以点开头。");
    if (
      !("version" in config) ||
      typeof config.version !== "string" ||
      byteLength(config.version) > 31 ||
      !semver.test(config.version)
    )
      throw new Error("version 需要 SemVer 格式，例如 1.0.0。");
    const metadata = config as AppConfig;
    for (const [field, limit] of [
      ["display_name", 128],
      ["description", 1024],
    ] as const) {
      const value = metadata[field];
      if (value !== undefined && (typeof value !== "string" || byteLength(value) > limit || value.includes("\0")))
        throw new Error(`${field} 必须是 ${limit} 字节以内的文本。`);
    }
    if (byteLength(project.files.find((f) => f.path === "app.json")!.content) > 32768)
      throw new Error("app.json 超过 32 KiB。");
    const entry = `${metadata.app_id}.lua`;
    if (!project.files.some((f) => f.path === entry))
      throw new Error(`缺少入口文件 ${entry}。修改 app_id 后请同步重命名入口文件。`);
    for (const file of project.files.filter((f) => f.path !== "app.json")) {
      bytes += byteLength(file.content);
      if (file.path !== entry && !file.path.startsWith(metadata.app_id + "/"))
        throw new Error(`资源文件必须位于 ${metadata.app_id}/ 目录内：${file.path}`);
      if (
        file.path === entry &&
        (byteLength(file.content) > 256 * 1024 || file.content.includes("\0") || file.content.startsWith("\x1bLua"))
      )
        throw new Error("Lua 入口必须是 256 KiB 以内的文本源码。");
    }
    if (bytes > 2 * 1024 * 1024) throw new Error("程序内容超过 2 MiB。");
    return { config: metadata, errors: [], bytes };
  } catch (error) {
    return {
      errors: [error instanceof Error ? error.message : "配置无效"],
      bytes,
    };
  }
}

export function downloadProject(project: Project): void {
  const blob = new Blob([JSON.stringify(project, null, 2)], {
    type: "application/json",
  });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = "workspace.litelink.json";
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
