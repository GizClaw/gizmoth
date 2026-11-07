import assert from "node:assert/strict";
import test from "node:test";
import { isThemePreference, resolveTheme } from "../src/theme.ts";
import { isLanguagePreference, resolveLanguage } from "../src/language.ts";

test("theme follows the system only when requested", () => {
  assert.equal(resolveTheme("system", true), "dark");
  assert.equal(resolveTheme("system", false), "light");
  assert.equal(resolveTheme("light", true), "light");
  assert.equal(resolveTheme("dark", false), "dark");
  for (const value of [null, undefined, "", "auto", "DARK", {}, true]) assert.equal(isThemePreference(value), false);
  for (const value of ["system", "light", "dark"]) assert.equal(isThemePreference(value), true);
});
test("app language overrides locale and system mode maps available translations", () => {
  for (const locale of ["zh", "zh-CN", "zh-Hant-TW", "ZH-hk", "zh_TW"])
    assert.equal(resolveLanguage("system", locale), "zh-CN");
  for (const locale of ["en-US", "fr-FR", "ja-JP", "", "zhong"]) assert.equal(resolveLanguage("system", locale), "en");
  assert.equal(resolveLanguage("en", "zh-CN"), "en");
  assert.equal(resolveLanguage("zh-CN", "en-US"), "zh-CN");
  for (const value of [null, "auto", "zh", "zh-TW", "fr-FR", {}]) assert.equal(isLanguagePreference(value), false);
  for (const value of ["system", "zh-CN", "en"]) assert.equal(isLanguagePreference(value), true);
});
