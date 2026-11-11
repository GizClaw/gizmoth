// Read-only catalog access for an ordinary browser. No native bridge, simulated
// installations, recipes or binaries are executed here. Every document belongs
// to one immutable commit of the public development-environment repository.
import type { DevEnvChip, DevEnvState, DevEnvToolchain, ToolchainCategory } from "./devenv.ts";

const REPOSITORY = "https://github.com/GizClaw/gizmoth-devenv";
const API_VERSION = "devenv.gizmoth.dev/v1alpha1";
const MAX_AGE = 12 * 60 * 60 * 1000;
const MAX_DOCUMENT = 2 * 1024 * 1024;
const idPattern = /^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/;
const categories = new Set<ToolchainCategory>(["compiler", "build-tool", "post-build", "debugger", "utility"]);

type Document = Record<string, unknown>;
type Fetch = (url: string, options?: RequestInit) => Promise<Response>;

function object(value: unknown): Document {
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error("开发环境目录格式无效，请刷新重试。");
  return value as Document;
}
function text(value: unknown, max = 2000): string {
  if (typeof value !== "string" || !value.trim() || value.length > max)
    throw new Error("开发环境目录格式无效，请刷新重试。");
  return value;
}
function ids(value: unknown): string[] {
  if (
    !Array.isArray(value) ||
    value.length > 256 ||
    !value.every((id) => typeof id === "string" && id.length <= 64 && idPattern.test(id))
  )
    throw new Error("开发环境目录格式无效，请刷新重试。");
  if (new Set(value).size !== value.length) throw new Error("开发环境目录包含重复条目。");
  return value;
}
function document(value: unknown, kind: string, id?: string): Document {
  const data = object(value);
  if (data.apiVersion !== API_VERSION || data.kind !== kind || (id !== undefined && data.id !== id))
    throw new Error("开发环境目录格式无效，请刷新重试。");
  return data;
}
function publication(data: Document): "planned" | "published" {
  if (data.status !== "planned" && data.status !== "published") throw new Error("开发环境目录格式无效，请刷新重试。");
  return data.status;
}
function version(data: Document): string | null {
  if (publication(data) === "planned") return null;
  const current = text(data.defaultVersion, 96);
  if (!Array.isArray(data.releases) || !data.releases.some((item) => object(item).version === current))
    throw new Error("开发环境目录默认版本未发布。");
  return current;
}
function homepage(value: unknown): string {
  const raw = text(value);
  const url = new URL(raw);
  if (url.protocol !== "https:" || url.username || url.password) throw new Error("开发环境目录格式无效，请刷新重试。");
  return raw;
}
function entryState(data: Document) {
  return publication(data) === "planned"
    ? { status: "planned" as const, blocked: { code: "planned" as const }, reason: "配方待补，暂时无法安装" }
    : { status: "available" as const, blocked: null, reason: null };
}

async function responseText(response: Response, limit: number): Promise<string> {
  if (!response.ok) {
    if (response.status === 403 || response.status === 429) throw new Error("GitHub 请求过于频繁，请稍后刷新重试。");
    throw new Error(`GitHub HTTP ${response.status}`);
  }
  const reader = response.body?.getReader();
  if (!reader) throw new Error("无法读取开发环境目录，请刷新重试。");
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let size = 0;
  let result = "";
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > limit) throw new Error("开发环境目录文件过大。");
      result += decoder.decode(value, { stream: true });
    }
    return result + decoder.decode();
  } finally {
    await reader.cancel();
    reader.releaseLock();
  }
}

/** A failed refresh never replaces a previously loaded catalog. */
export function createBrowserCatalog(fetcher: Fetch, now: () => number = Date.now) {
  let cached: DevEnvState | null = null;
  let pending: Promise<DevEnvState> | null = null;

  async function load(): Promise<DevEnvState> {
    const request = (url: string, options: RequestInit = {}) =>
      fetcher(url, { ...options, signal: AbortSignal.timeout(15_000), credentials: "omit" });
    const commit = (
      await responseText(
        await request("https://api.github.com/repos/GizClaw/gizmoth-devenv/commits/main", {
          headers: { Accept: "application/vnd.github.sha" },
        }),
        128,
      )
    ).trim();
    if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error("GitHub 返回的提交无效");
    const read = async (path: string) =>
      JSON.parse(
        await responseText(
          await request(`https://raw.githubusercontent.com/GizClaw/gizmoth-devenv/${commit}/${path}`),
          MAX_DOCUMENT,
        ),
      ) as unknown;
    const index = document(await read("registry.json"), "Registry");
    if (index.repository !== REPOSITORY) throw new Error("开发环境目录来源不匹配。");
    const chipIds = ids(index.chips);
    const sdkIds = ids(index.sdks);
    const toolchainIds = ids(index.toolchains);
    if (chipIds.length + sdkIds.length + toolchainIds.length > 256) throw new Error("开发环境目录条目过多。");
    const [chipDocs, sdkDocs, toolchainDocs] = await Promise.all([
      Promise.all(chipIds.map(async (id) => document(await read(`chips/${id}/chip.json`), "Chip", id))),
      Promise.all(sdkIds.map(async (id) => document(await read(`sdks/${id}/sdk.json`), "Sdk", id))),
      Promise.all(
        toolchainIds.map(async (id) => document(await read(`toolchains/${id}/toolchain.json`), "Toolchain", id)),
      ),
    ]);
    const sdks = new Map(sdkDocs.map((sdk) => [text(sdk.id), sdk]));
    const chips: DevEnvChip[] = chipDocs.map((data) => {
      const sdk = sdks.get(text(data.sdk));
      if (!sdk || ids(data.tools).some((id) => !toolchainIds.includes(id))) throw new Error("开发环境目录引用缺失。");
      return {
        id: text(data.id),
        name: text(data.name),
        vendor: text(data.vendor),
        description: text(data.description),
        homepage: homepage(data.homepage),
        sdk: text(sdk.name),
        notes: data.notes ? text(data.notes) : null,
        version: version(sdk),
        packages: [],
        installationId: null,
        path: null,
        reinstallable: false,
        ...entryState(data),
      };
    });
    const toolchains: DevEnvToolchain[] = toolchainDocs.map((data) => {
      const category = text(data.category) as ToolchainCategory;
      if (!categories.has(category)) throw new Error("开发环境目录格式无效，请刷新重试。");
      return {
        id: text(data.id),
        name: text(data.name),
        description: text(data.description),
        category,
        homepage: homepage(data.homepage),
        notes: data.notes ? text(data.notes) : null,
        version: version(data),
        ...entryState(data),
      };
    });
    return {
      host: null,
      registry: { repository: REPOSITORY, commit, fetchedAt: now() },
      error: null,
      chips,
      toolchains,
      installations: [],
      job: null,
    };
  }

  function refresh(): Promise<DevEnvState> {
    if (!pending) {
      pending = load()
        .then((state) => {
          cached = state;
          return state;
        })
        .finally(() => {
          pending = null;
        });
    }
    return pending;
  }
  return {
    state: () => (cached && now() - cached.registry!.fetchedAt < MAX_AGE ? Promise.resolve(cached) : refresh()),
    refresh,
  };
}

export const browserCatalog = createBrowserCatalog((url, options) => fetch(url, options));
