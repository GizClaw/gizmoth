import assert from "node:assert/strict";
import test from "node:test";
import { DROPPED_IMAGE_TEXT, fitHistory, fitWithin, isAcceptedImage } from "../src/image-attachments.ts";

test("images scale down to the longest-side limit and never scale up", () => {
  assert.deepEqual(fitWithin(4000, 3000), { width: 1568, height: 1176 });
  assert.deepEqual(fitWithin(800, 600), { width: 800, height: 600 });
  assert.deepEqual(fitWithin(10, 20000, 100), { width: 1, height: 100 });
});

test("only PNG, JPEG, GIF and WebP are accepted", () => {
  for (const type of ["image/png", "image/jpeg", "image/gif", "image/webp"]) assert.ok(isAcceptedImage(type));
  for (const type of ["image/svg+xml", "image/heic", "application/pdf", ""]) assert.ok(!isAcceptedImage(type));
});

test("saved history drops the oldest image data first and leaves the input untouched", () => {
  const image = (n: number) => ({ type: "image", mimeType: "image/png", data: "x".repeat(n) });
  const messages = [
    { role: "user", content: [image(600), { type: "text", text: "first" }] },
    { role: "assistant", content: [{ type: "text", text: "ok" }] },
    { role: "user", content: [image(600), { type: "text", text: "second" }] },
  ];
  const original = JSON.stringify(messages);
  assert.equal(fitHistory(messages, 10_000), messages, "under budget returns the same array");
  const fitted = fitHistory(messages, 1000);
  assert.equal(JSON.stringify(messages), original, "input is not mutated");
  assert.deepEqual(fitted[0].content[0], { type: "text", text: DROPPED_IMAGE_TEXT });
  assert.equal((fitted[2].content[0] as { type: string }).type, "image", "the newest image is kept");
  assert.ok(JSON.stringify(fitted).length <= 1000);
});
