import assert from "node:assert/strict";
import test from "node:test";
import type { Api, Model } from "@mariozechner/pi-ai";
import {
  contextTokens,
  effectiveThinkingLevel,
  formatTokens,
  parseThinkingLevel,
  supportedThinkingLevels,
} from "../src/agent-thinking.ts";

const reasoning = { reasoning: true } as Model<Api>;
test("reasoning choices follow SDK capability mappings and reject stored invalid values", () => {
  assert.deepEqual(supportedThinkingLevels(reasoning), ["off", "minimal", "low", "medium", "high"]);
  assert.equal(effectiveThinkingLevel(reasoning, "high"), "high");
  assert.equal(parseThinkingLevel("invented"), "off");
  assert.equal(parseThinkingLevel(null), "off");
  assert.equal(parseThinkingLevel("toString"), "off");
  const highest = { ...reasoning, thinkingLevelMap: { xhigh: "xhigh", minimal: null } } as Model<Api>;
  assert.ok(supportedThinkingLevels(highest).includes("xhigh"));
  assert.ok(!supportedThinkingLevels(highest).includes("minimal"));
  assert.equal(effectiveThinkingLevel(highest, "minimal"), "low");
  assert.equal(effectiveThinkingLevel(reasoning, "xhigh"), "high");
});
test("non-reasoning or missing models never retain an unsupported effort", () => {
  const plain = { reasoning: false } as Model<Api>;
  assert.deepEqual(supportedThinkingLevels(plain), ["off"]);
  assert.equal(effectiveThinkingLevel(plain, "high"), "off");
  assert.equal(effectiveThinkingLevel(undefined, "medium"), "off");
});

test("token counts read compactly for model rows and the context meter", () => {
  assert.equal(formatTokens(0), "0");
  assert.equal(formatTokens(812), "812");
  assert.equal(formatTokens(8192), "8.2K");
  assert.equal(formatTokens(200000), "200K");
  assert.equal(formatTokens(1000000), "1M");
  assert.equal(formatTokens(1_500_000), "1.5M");
  assert.equal(formatTokens(Number.NaN), "0");
});
test("context usage comes only from the latest assistant turn that reported it", () => {
  assert.equal(contextTokens([]), undefined);
  assert.equal(contextTokens([{ role: "user" }, { role: "assistant", usage: { totalTokens: 0 } }]), undefined);
  assert.equal(
    contextTokens([
      { role: "assistant", usage: { totalTokens: 1200 } },
      { role: "toolResult" },
      { role: "assistant", usage: { totalTokens: 4800 } },
      { role: "assistant", usage: {} },
    ]),
    4800,
  );
});

test("provider-declared levels replace generic choices and reach the request unchanged", async () => {
  const { parseModelList, toPiModel } = await import("../src/providers.ts");
  const { thinkingRequest } = await import("../src/agent-thinking.ts");
  const models = parseModelList("openai-completions", {
    data: [
      { id: "kimi", reasoning: true, reasoning_levels: ["low", "high", "max"] },
      {
        id: "sol",
        reasoning: true,
        supported_reasoning_levels: [
          { effort: "LOW" },
          { effort: "medium" },
          { effort: "high" },
          { effort: "xhigh" },
          { effort: "max" },
          { effort: "ultra" },
          { effort: "<script>" },
        ],
      },
      { id: "doubao", reasoning: false, reasoning_levels: [] },
    ],
  });
  const [kimi, sol, doubao] = ["kimi", "sol", "doubao"].map((id) => models.find((model) => model.id === id)!);
  assert.deepEqual(sol.reasoningLevels, ["low", "medium", "high", "xhigh", "max", "ultra"]);
  assert.equal("reasoningLevels" in doubao, false);
  const provider = {
    id: "magpie",
    kind: "magpie",
    name: "magpie",
    api: "openai-completions" as const,
    baseUrl: "http://127.0.0.1:3425/v1",
    hasKey: false,
    models: [],
  };
  const solModel = toPiModel(provider, sol);
  const kimiModel = toPiModel(provider, kimi);
  assert.deepEqual(supportedThinkingLevels(solModel), ["off", "low", "medium", "high", "xhigh", "max", "ultra"]);
  assert.deepEqual(supportedThinkingLevels(kimiModel), ["off", "low", "high", "max"]);
  assert.deepEqual(supportedThinkingLevels(toPiModel(provider, doubao)), ["off"]);
  // Each choice travels as exactly one advertised provider value.
  for (const effort of ["low", "medium", "high", "xhigh", "max", "ultra"]) {
    const { level, model } = thinkingRequest(solModel, effort);
    assert.equal(model.thinkingLevelMap?.[level], effort, effort);
  }
  assert.equal(thinkingRequest(solModel, "off").level, "off");
  // Switching models keeps the nearest available level rather than an unsupported one.
  assert.equal(effectiveThinkingLevel(kimiModel, "ultra"), "max");
  assert.equal(effectiveThinkingLevel(kimiModel, "medium"), "high");
  assert.equal(effectiveThinkingLevel(kimiModel, "minimal"), "low");
  assert.equal(parseThinkingLevel("ultra"), "ultra");
});
