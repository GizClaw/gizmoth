export type LanguagePreference = "system" | "zh-CN" | "en";
export type AppLanguage = "zh-CN" | "en";
export const languageStorageKey = "gizmoth.language";
export const isLanguagePreference = (value: unknown): value is LanguagePreference =>
  value === "system" || value === "zh-CN" || value === "en";
export function resolveLanguage(preference: LanguagePreference, systemLocale = "en"): AppLanguage {
  return preference === "system"
    ? systemLocale.split(/[-_]/, 1)[0].toLowerCase() === "zh"
      ? "zh-CN"
      : "en"
    : preference;
}
function readPreference(): LanguagePreference {
  try {
    const value = localStorage.getItem(languageStorageKey);
    return isLanguagePreference(value) ? value : "system";
  } catch {
    return "system";
  }
}
const systemLocale = () => (typeof navigator === "undefined" ? "en" : navigator.language);
let snapshot = { preference: readPreference(), language: "en" as AppLanguage };
snapshot.language = resolveLanguage(snapshot.preference, systemLocale());
const subscribers = new Set<() => void>();
function apply(preference: LanguagePreference) {
  const language = resolveLanguage(preference, systemLocale());
  if (typeof document !== "undefined") {
    document.documentElement.lang = language;
    document.documentElement.dataset.language = language;
  }
  if (snapshot.preference === preference && snapshot.language === language) return;
  snapshot = { preference, language };
  subscribers.forEach((listener) => listener());
}
export const getLanguageSnapshot = () => snapshot;
export function subscribeLanguage(listener: () => void) {
  subscribers.add(listener);
  return () => {
    subscribers.delete(listener);
  };
}
export function syncLanguagePreference(preference: LanguagePreference) {
  try {
    localStorage.setItem(languageStorageKey, preference);
  } catch {
    /* Native preferences do not depend on the first-paint cache. */
  }
  apply(preference);
}
export function saveBrowserLanguage(preference: LanguagePreference) {
  localStorage.setItem(languageStorageKey, preference);
  apply(preference);
}
if (typeof window !== "undefined") {
  window.addEventListener("languagechange", () => apply(snapshot.preference));
  window.addEventListener("storage", (event) => {
    if (event.key === languageStorageKey || event.key === null) apply(readPreference());
  });
}
apply(snapshot.preference);
