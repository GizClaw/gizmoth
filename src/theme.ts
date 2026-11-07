export type ThemePreference = "system" | "light" | "dark";
export type AppTheme = "light" | "dark";
export const themeStorageKey = "gizmoth.theme";
export const isThemePreference = (value: unknown): value is ThemePreference =>
  value === "system" || value === "light" || value === "dark";
export function resolveTheme(preference: ThemePreference, systemDark: boolean): AppTheme {
  return preference === "system" ? (systemDark ? "dark" : "light") : preference;
}
const media = typeof window === "undefined" ? undefined : window.matchMedia("(prefers-color-scheme: dark)");
function readPreference(): ThemePreference {
  try {
    const value = localStorage.getItem(themeStorageKey);
    return isThemePreference(value) ? value : "system";
  } catch {
    return "system";
  }
}
let snapshot = { preference: readPreference(), theme: "light" as AppTheme };
snapshot.theme = resolveTheme(snapshot.preference, media?.matches ?? false);
const subscribers = new Set<() => void>();
function apply(preference: ThemePreference) {
  const theme = resolveTheme(preference, media?.matches ?? false);
  if (typeof document !== "undefined") {
    document.documentElement.dataset.theme = theme;
    document.documentElement.style.colorScheme = theme;
  }
  if (snapshot.preference === preference && snapshot.theme === theme) return;
  snapshot = { preference, theme };
  subscribers.forEach((listener) => listener());
}
export const getThemeSnapshot = () => snapshot;
export function subscribeTheme(listener: () => void) {
  subscribers.add(listener);
  return () => {
    subscribers.delete(listener);
  };
}
/** Native preferences are authoritative; local storage is only a first-paint cache. */
export function syncThemePreference(preference: ThemePreference) {
  try {
    localStorage.setItem(themeStorageKey, preference);
  } catch {
    /* A native preference remains valid without the cache. */
  }
  apply(preference);
}
export function saveBrowserTheme(preference: ThemePreference) {
  localStorage.setItem(themeStorageKey, preference);
  apply(preference);
}
media?.addEventListener("change", () => apply(snapshot.preference));
if (typeof window !== "undefined")
  window.addEventListener("storage", (event) => {
    if (event.key === themeStorageKey || event.key === null) apply(readPreference());
  });
apply(snapshot.preference);
