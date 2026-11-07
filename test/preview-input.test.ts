import test from "node:test";
import assert from "node:assert/strict";
import { createPreviewInput, previewKey, type PreviewButton } from "../src/preview-input.ts";

test("preview shortcuts preserve hardware semantics without claiming up/down", () => {
  assert.equal(previewKey({ code: "ArrowLeft" }), "left");
  assert.equal(previewKey({ code: "ArrowRight" }), "right");
  assert.equal(previewKey({ code: "Enter" }), "ok");
  assert.equal(previewKey({ code: "Escape" }), "back");
  assert.equal(previewKey({ code: "KeyB" }), "power");
  assert.equal(previewKey({ code: "ArrowUp" }), undefined);
  assert.equal(previewKey({ code: "ArrowDown" }), undefined);
  for (const modifiers of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }, { isComposing: true }])
    assert.equal(previewKey({ code: "ArrowLeft", ...modifiers }), undefined);
});

test("held input retains independent pointer and keyboard sources", () => {
  const events: string[] = [];
  const active = new Map<string, PreviewButton>();
  const input = createPreviewInput({
    press(button, source) {
      events.push(`down:${source}:${button}`);
      active.set(source, button);
    },
    release(button, source) {
      events.push(`up:${source}:${button}`);
      active.delete(source);
    },
  });
  input.press("left", "pointer:1");
  input.press("left", "key:ArrowLeft");
  input.press("ok", "pointer:2");
  input.press("left", "key:ArrowLeft"); // Browser key repeat must not duplicate the press.
  assert.equal(events.length, 3);
  input.release("pointer:1");
  assert.deepEqual([...active.values()], ["left", "ok"]);
  input.releaseAll(); // Blur/cancel/unmount releases every source owned by the host.
  assert.equal(active.size, 0);
  assert.equal(events.length, 6);
  input.releaseAll();
  input.release("pointer:missing");
  assert.equal(events.length, 6);
});

test("reusing an input source releases its previous device key first", () => {
  const events: string[] = [];
  const input = createPreviewInput({
    press: (button) => events.push(`down:${button}`),
    release: (button) => events.push(`up:${button}`),
  });
  input.press("back", "pointer:1");
  input.press("ok", "pointer:1");
  input.release("pointer:1");
  assert.deepEqual(events, ["down:back", "up:back", "down:ok", "up:ok"]);
});
