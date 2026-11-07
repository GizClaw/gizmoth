import test from "node:test";
import assert from "node:assert/strict";
import type { ModelFirmware, ModelsProvider } from "@gizclaw/models-player";
import { previewFirmwareProvider } from "../src/preview-firmware.ts";

const choices: ModelFirmware[] = [
  { id: "trial", name: "Trial", model: "h106-tiga", board: "tiga", abi: "gizos-web-v1" },
  { id: "game", name: "Game firmware", model: "h106-tiga", board: "tiga", abi: "gizos-web-v1" },
];
const context = { signal: new AbortController().signal };

test("installation admits only the selected firmware but preserves all UI choices", async () => {
  let visible: readonly ModelFirmware[] = [];
  const base: ModelsProvider = {
    async listFirmwares() {
      return choices;
    },
    async resolveFirmware(firmware, input) {
      assert.equal(firmware.id, "game");
      assert.equal(input, context);
      return { entryUrl: "https://example.test/firmware.html", abi: "gizos-web-v1", board: "tiga" };
    },
  };
  const provider = previewFirmwareProvider(base, "game", (items) => {
    visible = items;
  });
  const admitted = await provider.listFirmwares("h106-tiga", context);
  assert.deepEqual(
    admitted.map((firmware) => firmware.id),
    ["game"],
  );
  assert.deepEqual(
    visible.map((firmware) => firmware.id),
    ["trial", "game"],
  );
  assert.equal(choices[0].id, "trial");
  await provider.resolveFirmware(admitted[0], context);
});

test("missing or mismatched firmware cannot silently fall back to a default", async () => {
  const base: ModelsProvider = {
    async listFirmwares() {
      return choices;
    },
    async resolveFirmware() {
      throw new Error("not called");
    },
  };
  await assert.rejects(previewFirmwareProvider(base, "removed-release", () => {}).listFirmwares("h106-tiga", context));
  await assert.rejects(previewFirmwareProvider(base, "game", () => {}).listFirmwares("h106-zero", context), /其他型号/);
});

test("model-only preview discovers compatible choices without selecting a firmware", async () => {
  const base: ModelsProvider = {
    async listFirmwares() {
      return [];
    },
    async resolveFirmware() {
      throw new Error("must not install");
    },
  };
  assert.deepEqual(await previewFirmwareProvider(base, null, () => {}).listFirmwares("h106-tiga", context), []);
});
