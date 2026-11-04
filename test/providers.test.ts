import assert from "node:assert/strict";
import test from "node:test";
import { parseModelList, resolveModel, slug, toPiModel } from "../src/providers.ts";

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
