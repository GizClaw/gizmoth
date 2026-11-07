import { resolveLanguage } from "../language.ts";
import type { DocumentationLanguage } from "./catalog.ts";

/** Resolve available documentation translations using the App's locale rules. */
export function documentationLanguage(locale = "en"): DocumentationLanguage {
  return resolveLanguage("system", locale);
}
