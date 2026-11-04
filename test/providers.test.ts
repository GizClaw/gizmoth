import assert from "node:assert/strict";
import test from "node:test";
import {
  deviceProviderBase,
  mergeModelCatalogue,
  parseModelList,
  pickedModels,
  resolveModel,
  slug,
  toPiModel,
} from "../src/providers.ts";

test("parses OpenAI-compatible model lists including magpie metadata", () => {
  const models = parseModelList("openai-completions", {
    data: [
      {
        id: "auto/claude-haiku-5-5",
        display_name: "Claude Haiku 5.5",
        reasoning: true,
        context_window: 1000000,
        max_output_tokens: 128000,
        modalities: { input: ["text", "image"] },
      },
      { id: "gpt-x" },
      { id: "gpt-x" },
    ],
  });
  assert.equal(models.length, 2);
  assert.deepEqual(models[0], {
    id: "auto/claude-haiku-5-5",
    name: "Claude Haiku 5.5",
    reasoning: true,
    images: true,
    contextWindow: 1000000,
    maxTokens: 128000,
  });
  assert.equal(models[1].contextWindow, 128000);
});

test("parses Gemini lists and skips models without generateContent", () => {
  const models = parseModelList("google-generative-ai", {
    models: [
      {
        name: "models/gemini-x",
        displayName: "Gemini X",
        supportedGenerationMethods: ["generateContent"],
        inputTokenLimit: 1048576,
      },
      { name: "models/embed", supportedGenerationMethods: ["embedContent"] },
    ],
  });
  assert.deepEqual(
    models.map((m) => m.id),
    ["gemini-x"],
  );
});

test("maps providers to pi models and resolves references", () => {
  const provider = {
    id: "magpie",
    kind: "magpie",
    name: "magpie",
    api: "openai-completions" as const,
    baseUrl: "http://127.0.0.1:3425/v1/",
    hasKey: false,
    models: parseModelList("openai-completions", { data: [{ id: "m1" }] }),
  };
  const resolved = resolveModel([provider], "magpie/m1");
  assert.ok(resolved);
  const model = toPiModel(resolved.provider, resolved.model);
  assert.equal(model.baseUrl, "http://127.0.0.1:3425/v1");
  assert.equal(model.provider, "magpie");
  assert.equal(resolveModel([provider], undefined)?.model.id, "m1");
  assert.equal(resolveModel([provider], "other/x"), undefined);
  assert.equal(slug("My Gateway!"), "my-gateway");
});

test("only picked models are available, including providers saved before selection existed", () => {
  const provider = {
    id: "device",
    kind: "device",
    name: "Desk",
    api: "openai-completions" as const,
    baseUrl: "https://gizclaw.example/openai/v1",
    hasKey: true,
    models: parseModelList("openai-completions", { data: [{ id: "old-model" }, { id: "disabled" }] }),
  };
  provider.models.find((m) => m.id === "disabled")!.enabled = false;
  assert.equal(pickedModels(provider).length, 1);
  assert.equal(resolveModel([provider], "device/disabled"), undefined);
  assert.equal(resolveModel([provider], undefined)?.model.id, "old-model");
});

test("refresh preserves picks, deselections and manual models without enabling new catalogue entries", () => {
  const before = parseModelList("openai-completions", { data: [{ id: "picked" }, { id: "off" }, { id: "manual" }] });
  before[before.findIndex((m) => m.id === "off")].enabled = false;
  const merged = mergeModelCatalogue(
    before,
    parseModelList("openai-completions", { data: [{ id: "picked" }, { id: "off" }, { id: "new" }] }),
  );
  assert.deepEqual(
    merged.filter((m) => m.enabled !== false).map((m) => m.id),
    ["picked", "manual"],
  );
  assert.equal(merged.find((m) => m.id === "new")?.enabled, false);
  assert.equal(mergeModelCatalogue([], before).filter((m) => m.enabled).length, 0);
});

test("device Providers use their own gateway and omit unsupported OpenAI options", () => {
  const provider = {
    id: "desk",
    kind: "device",
    deviceId: "paired",
    name: "Desk",
    api: "openai-completions" as const,
    baseUrl: deviceProviderBase("https://gizclaw.example/"),
    hasKey: true,
    models: parseModelList("openai-completions", { data: [{ id: "chat" }] }),
  };
  assert.equal(provider.baseUrl, "https://gizclaw.example/openai/v1");
  const model = toPiModel(provider, provider.models[0]);
  assert.equal(model.api, "openai-completions");
  assert.ok(model.compat && "supportsStore" in model.compat && "supportsStrictMode" in model.compat);
  assert.equal(model.compat.supportsStore, false);
  assert.equal(model.compat.supportsStrictMode, false);
});
