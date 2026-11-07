import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isThemePreference, saveBrowserTheme, syncThemePreference, type ThemePreference } from "./theme";
import { isLanguagePreference, saveBrowserLanguage, syncLanguagePreference, type LanguagePreference } from "./language";

let ready: Promise<void> | undefined;
async function connectPreference<T>(
  command: string,
  event: string,
  valid: (value: unknown) => value is T,
  apply: (value: T) => void,
) {
  let revision = 0;
  await listen<unknown>(event, ({ payload }) => {
    if (!valid(payload)) return;
    revision++;
    apply(payload);
  });
  const before = revision;
  const value = await invoke<unknown>(command);
  // An event received during the read has the newer authoritative value.
  if (before === revision && valid(value)) apply(value);
}
export function initializeAppearance(): Promise<void> {
  if (!isTauri()) return Promise.resolve();
  return (ready ??= Promise.all([
    connectPreference("theme_preference", "desktop-theme-changed", isThemePreference, syncThemePreference),
    connectPreference("language_preference", "desktop-language-changed", isLanguagePreference, syncLanguagePreference),
  ]).then(
    () => {},
    (error) => {
      console.error("Could not synchronize app preferences", error);
    },
  ));
}
export async function saveThemePreference(preference: ThemePreference) {
  if (!isTauri()) {
    saveBrowserTheme(preference);
    return;
  }
  const value = await invoke<unknown>("set_theme_preference", { preference });
  if (!isThemePreference(value)) throw new Error("Invalid theme preference");
  syncThemePreference(value);
}
export async function saveLanguagePreference(preference: LanguagePreference) {
  if (!isTauri()) {
    saveBrowserLanguage(preference);
    return;
  }
  const value = await invoke<unknown>("set_language_preference", { preference });
  if (!isLanguagePreference(value)) throw new Error("Invalid language preference");
  syncLanguagePreference(value);
}
