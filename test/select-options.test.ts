import assert from "node:assert/strict";
import test from "node:test";
import { matchSelectValue, nextSelectValue, selectPopupPosition } from "../src/select-options.ts";

const options = [
  { value: "", label: "None" },
  { value: "a", label: "Alpha" },
  { value: "b", label: "Beta", disabled: true },
  { value: "c", label: "Charlie" },
];

test("dropdown keyboard navigation wraps, skips disabled choices and preserves empty values", () => {
  assert.equal(nextSelectValue(options, "a", 1), "c");
  assert.equal(nextSelectValue(options, "c", 1), "");
  assert.equal(nextSelectValue(options, "", -1), "c");
  assert.equal(nextSelectValue(options, "missing", 1), "");
  assert.equal(nextSelectValue(options, "missing", -1), "c");
  assert.equal(nextSelectValue([], "", 1), undefined);
  assert.equal(nextSelectValue([{ value: "x", label: "X", disabled: true }], "x", 1), undefined);
});

test("dropdown typeahead matches labels case-insensitively and never chooses a disabled item", () => {
  assert.equal(matchSelectValue(options, "AL", ""), "a");
  assert.equal(matchSelectValue(options, "b", "a"), undefined);
  assert.equal(matchSelectValue(options, "n", "a"), "");
  const repeated = [
    { value: "a", label: "Alpha" },
    { value: "b", label: "Amber" },
  ];
  assert.equal(matchSelectValue(repeated, "a", "a"), "b");
  assert.equal(matchSelectValue(repeated, "a", "b"), "a");
  const prefix = [
    { value: "apple", label: "Apple" },
    { value: "apricot", label: "Apricot" },
    { value: "banana", label: "Banana" },
  ];
  assert.equal(matchSelectValue(prefix, "a", "banana"), "apple");
  assert.equal(matchSelectValue(prefix, "ap", "apple"), "apple");
  assert.equal(matchSelectValue(prefix, "a", "apple"), "apricot");
});

test("dropdown placement flips above near the bottom and clamps within the viewport", () => {
  const viewport = { width: 390, height: 844 };
  const below = selectPopupPosition({ left: 10, top: 40, bottom: 68, width: 88 }, viewport, 2);
  assert.ok("top" in below);
  assert.equal(below.top, 72);
  assert.equal(below.width, 160);
  const above = selectPopupPosition({ left: 340, top: 760, bottom: 788, width: 88 }, viewport, 20);
  assert.ok("bottom" in above);
  assert.equal(above.bottom, 88);
  assert.equal(above.left + above.width, 382);
  assert.ok(above.maxHeight <= 300);
  const small = selectPopupPosition({ left: 12, top: 70, bottom: 98, width: 400 }, { width: 200, height: 120 }, 20);
  assert.ok(small.left >= 8 && small.left + small.width <= 192);
  assert.equal(small.maxHeight, 58);
});
