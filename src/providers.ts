import { getModels, type Api, type Model } from "@mariozechner/pi-ai";
import type { Provider, ProviderApi, ProviderModel } from "./bridge";
import { normalizeReasoningLevels } from "./agent-thinking.ts";

export interface Preset {
  kind: string;
  name: string;
  api: ProviderApi;
  baseUrl: string;
  needsKey: boolean;
  hint: string;
}

export const presets: Preset[] = [
  {
    kind: "magpie",
    name: "magpie",
    api: "openai-completions",
    baseUrl: "http://127.0.0.1:3425/v1",
    needsKey: false,
    hint: "使用本机 magpie 网关路由的账号与模型",
  },
  {
    kind: "anthropic",
    name: "Anthropic",
    api: "anthropic-messages",
    baseUrl: "https://api.anthropic.com",
    needsKey: true,
    hint: "Claude 模型，使用 Anthropic API key",
  },
  {
    kind: "openai",
    name: "OpenAI",
    api: "openai-responses",
    baseUrl: "https://api.openai.com/v1",
    needsKey: true,
    hint: "GPT 模型，使用 OpenAI API key",
  },
  {
    kind: "google",
    name: "Google Gemini",
    api: "google-generative-ai",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta",
    needsKey: true,
    hint: "Gemini 模型，使用 Google AI Studio key",
  },
  {
    kind: "openrouter",
    name: "OpenRouter",
    api: "openai-completions",
    baseUrl: "https://openrouter.ai/api/v1",
    needsKey: true,
    hint: "通过 OpenRouter 访问多家模型",
  },
  {
    kind: "deepseek",
    name: "DeepSeek",
    api: "openai-completions",
    baseUrl: "https://api.deepseek.com",
    needsKey: true,
    hint: "DeepSeek 模型",
  },
  {
    kind: "custom",
    name: "自定义",
    api: "openai-completions",
    baseUrl: "https://",
    needsKey: false,
    hint: "任意 OpenAI 或 Anthropic 兼容接口",
  },
];

export const apiLabels: Record<ProviderApi, string> = {
  "openai-completions": "OpenAI Chat Completions",
  "openai-responses": "OpenAI Responses",
  "anthropic-messages": "Anthropic Messages",
  "google-generative-ai": "Google Generative AI",
};

export function presetOf(kind: string) {
  return presets.find((p) => p.kind === kind) ?? presets[presets.length - 1];
}

export function slug(text: string) {
  return (
    text
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 40) || "provider"
  );
}

/** Model reference persisted with a project: `<providerId>/<modelId>`. */
export const modelRef = (provider: Provider, model: ProviderModel) => `${provider.id}/${model.id}`;

export function resolveModel(providers: Provider[], ref: string | undefined) {
  for (const provider of providers)
    for (const model of provider.models) if (!ref || modelRef(provider, model) === ref) return { provider, model };
  return undefined;
}

export function toPiModel(provider: Provider, model: ProviderModel): Model<Api> {
  const catalog =
    provider.api === "anthropic-messages"
      ? getModels("anthropic")
      : provider.api === "google-generative-ai"
        ? getModels("google")
        : getModels("openai");
  const known = catalog.find((entry) => entry.id === model.id);
  return {
    id: model.id,
    name: model.name || model.id,
    api: provider.api,
    provider: provider.id,
    baseUrl: provider.baseUrl.replace(/\/+$/, ""),
    reasoning: model.reasoning,
    // Levels the provider declared replace the SDK's guess for known ids.
    reasoningLevels: model.reasoning ? normalizeReasoningLevels(model.reasoningLevels) : undefined,
    thinkingLevelMap: model.reasoning && !model.reasoningLevels?.length ? known?.thinkingLevelMap : undefined,
    // Provider model lists rarely report modalities; fall back to the SDK catalog for known ids.
    input: model.images || known?.input.includes("image") ? ["text", "image"] : ["text"],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: model.contextWindow,
    maxTokens: model.maxTokens,
  } as Model<Api>;
}

const num = (value: unknown, fallback: number) =>
  typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.floor(value) : fallback;

/** Parse an OpenAI-, Anthropic- or Gemini-style model list into provider models. */
export function parseModelList(api: ProviderApi, body: unknown): ProviderModel[] {
  const root = body as Record<string, unknown> | null;
  const rows = (root && (Array.isArray(root.data) ? root.data : Array.isArray(root.models) ? root.models : null)) ?? [];
  const out: ProviderModel[] = [];
  const seen = new Set<string>();
  for (const raw of rows as Record<string, unknown>[]) {
    if (!raw || typeof raw !== "object") continue;
    let id = typeof raw.id === "string" ? raw.id : typeof raw.name === "string" ? raw.name : "";
    if (api === "google-generative-ai") {
      id = id.replace(/^models\//, "");
      const methods = raw.supportedGenerationMethods;
      if (Array.isArray(methods) && !methods.includes("generateContent")) continue;
    }
    if (!id || seen.has(id) || id.length > 200) continue;
    seen.add(id);
    const modalities = (raw.modalities as { input?: unknown } | undefined)?.input;
    const reasoningLevels = normalizeReasoningLevels(
      Array.isArray(raw.reasoning_levels) ? raw.reasoning_levels : raw.supported_reasoning_levels,
    );
    out.push({
      id,
      name:
        typeof raw.display_name === "string"
          ? raw.display_name
          : typeof raw.displayName === "string"
            ? raw.displayName
            : id,
      reasoning: raw.reasoning === true || raw.thinking === true,
      ...(reasoningLevels.length ? { reasoningLevels } : {}),
      images: Array.isArray(modalities) ? modalities.includes("image") : false,
      contextWindow: num(
        raw.context_window ?? raw.context_length ?? raw.inputTokenLimit ?? raw.max_input_tokens,
        128_000,
      ),
      maxTokens: num(
        raw.max_output_tokens ??
          raw.outputTokenLimit ??
          (raw.top_provider as { max_completion_tokens?: number })?.max_completion_tokens,
        16_384,
      ),
    });
  }
  return out.sort((a, b) => a.id.localeCompare(b.id));
}

export async function fetchModels(api: ProviderApi, baseUrl: string, apiKey: string | undefined) {
  const base = baseUrl.replace(/\/+$/, "");
  const headers: Record<string, string> = {};
  let url = `${base}/models`;
  if (api === "anthropic-messages") {
    url = `${base}/v1/models?limit=1000`;
    if (apiKey) headers["x-api-key"] = apiKey;
    headers["anthropic-version"] = "2023-06-01";
  } else if (api === "google-generative-ai") {
    if (apiKey) headers["x-goog-api-key"] = apiKey;
    url = `${base}/models?pageSize=1000`;
  } else if (apiKey) headers.authorization = `Bearer ${apiKey}`;
  const response = await fetch(url, { headers });
  if (!response.ok) throw new Error(`读取模型列表失败：HTTP ${response.status}`);
  const models = parseModelList(api, await response.json());
  if (!models.length) throw new Error("接口没有返回可用模型，请手动添加模型 ID。");
  return models;
}
