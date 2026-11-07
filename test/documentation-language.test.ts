import assert from "node:assert/strict";
import test from "node:test";
import { documentationLanguage } from "../src/gizos-docs/language.ts";

test("documentation maps the system locale to an available translation", () => {
  for (const locale of ["zh", "zh-CN", "zh-Hans-CN", "zh-Hant-TW", "zh-HK", "ZH-cn", "zh_TW"]) {
    assert.equal(documentationLanguage(locale), "zh-CN", locale);
  }
  for (const locale of [undefined, "", "en", "en-US", "en-GB", "fr-FR", "ja-JP", "zhong"]) {
    assert.equal(documentationLanguage(locale), "en", locale);
  }
});
