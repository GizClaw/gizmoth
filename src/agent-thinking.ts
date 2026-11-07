import { getSupportedThinkingLevels, type Api, type Model } from "@mariozechner/pi-ai";
import type { Agent, ThinkingLevel } from "@mariozechner/pi-agent-core";

/** App thinking levels, lowest to highest. Providers may advertise any subset beyond the SDK's five. */
export const thinkingOrder = ["off", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"] as const;
export type Effort = (typeof thinkingOrder)[number];

export const thinkingLabels: Record<Effort, string> = {
  off: "关闭",
  minimal: "最低",
  low: "低",
  medium: "中",
  high: "高",
  xhigh: "很高",
  max: "最高",
  ultra: "极限",
};

const isEffort = (value: unknown): value is Effort =>
  typeof value === "string" && (thinkingOrder as readonly string[]).includes(value);

export function parseThinkingLevel(value: unknown): Effort {
  return isEffort(value) ? value : "off";
}

/** Levels the provider's model list advertised, kept only when they are app-known names. */
export function normalizeReasoningLevels(values: unknown): Effort[] {
  if (!Array.isArray(values)) return [];
  const levels = values
    .map((value) => (typeof value === "string" ? value : (value as { effort?: unknown } | null)?.effort))
    .filter((value): value is string => typeof value === "string")
    .map((value) => value.toLowerCase())
    .filter((value): value is Effort => isEffort(value) && value !== "off");
  return thinkingOrder.filter((level) => levels.includes(level));
}

export type LeveledModel = Model<Api> & { reasoningLevels?: readonly Effort[] };

/** Provider-declared levels win; otherwise the SDK's capability mapping for known model ids. */
export function supportedThinkingLevels(model?: LeveledModel): Effort[] {
  if (!model) return ["off"];
  if (model.reasoning && model.reasoningLevels?.length) return ["off", ...model.reasoningLevels];
  return getSupportedThinkingLevels(model);
}

const rank = (level: Effort) => thinkingOrder.indexOf(level);

/** Keep the requested level when available, else the nearest higher, else the nearest lower one. */
export function effectiveThinkingLevel(model: LeveledModel | undefined, requested: unknown): Effort {
  if (!model) return "off";
  const levels = supportedThinkingLevels(model);
  const want = parseThinkingLevel(requested);
  if (levels.includes(want)) return want;
  const higher = levels.find((level) => rank(level) > rank(want));
  const lower = [...levels].reverse().find((level) => rank(level) < rank(want));
  return higher ?? lower ?? "off";
}

/**
 * The SDK only has five reasoning slots. A provider level travels in one slot through the
 * model's thinkingLevelMap, so the request carries exactly the advertised value (e.g. "ultra").
 */
export function thinkingRequest(model: LeveledModel, requested: unknown): { level: ThinkingLevel; model: Model<Api> } {
  const effort = effectiveThinkingLevel(model, requested);
  if (!model.reasoningLevels?.length || effort === "off") return { level: effort as ThinkingLevel, model };
  const slot: ThinkingLevel = ["minimal", "low", "medium", "high"].includes(effort)
    ? (effort as ThinkingLevel)
    : "xhigh";
  const thinkingLevelMap = { minimal: null, low: null, medium: null, high: null, xhigh: null, [slot]: effort };
  return { level: slot, model: { ...model, thinkingLevelMap } as Model<Api> };
}

export function applyThinking(agent: Agent, requested: unknown) {
  const { level, model } = thinkingRequest(agent.state.model as LeveledModel, requested);
  agent.state.model = model;
  agent.state.thinkingLevel = level;
  return level;
}

/** Compact token counts for menus: 8K, 128K, 1M, 1.5M. */
export function formatTokens(count: number) {
  if (!Number.isFinite(count) || count <= 0) return "0";
  if (count >= 1_000_000) return `${Number((count / 1_000_000).toFixed(1))}M`;
  if (count >= 1000) return `${Number((count / 1000).toFixed(count >= 100_000 ? 0 : 1))}K`;
  return String(Math.round(count));
}

export interface ContextUsage {
  total: number;
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

const count = (value: unknown) => (typeof value === "number" && Number.isFinite(value) && value > 0 ? value : 0);

/** Usage the latest assistant turn reported for the whole context, or undefined when unknown. */
export function contextUsage(messages: readonly unknown[]): ContextUsage | undefined {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i] as { role?: string; usage?: Record<string, unknown> };
    if (message?.role !== "assistant") continue;
    const usage = message.usage ?? {};
    const total = count(usage.totalTokens);
    if (total)
      return {
        total,
        input: count(usage.input),
        output: count(usage.output),
        cacheRead: count(usage.cacheRead),
        cacheWrite: count(usage.cacheWrite),
      };
  }
  return undefined;
}

export function contextTokens(messages: readonly unknown[]) {
  return contextUsage(messages)?.total;
}
