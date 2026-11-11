import { useSyncExternalStore } from "react";
import { getLanguageSnapshot, subscribeLanguage } from "./language";
import { enCommon } from "./locales/en-common";
import { enWorkspace } from "./locales/en-workspace";
import { enDevices } from "./locales/en-devices";
import { enSerial } from "./locales/en-serial";
import { enAgents } from "./locales/en-agents";

export type TranslationValues = Record<string, string | number>;
const english: Record<string, string> = { ...enAgents, ...enSerial, ...enDevices, ...enWorkspace, ...enCommon };
/** Chinese source messages are stable keys; values remain plain text rendered by React. */
export function t(message: string, values: TranslationValues = {}): string {
  const template = getLanguageSnapshot().language === "en" ? (english[message] ?? message) : message;
  return template.replace(/\{(\w+)\}/g, (token, name: string) =>
    Object.hasOwn(values, name) ? String(values[name]) : token,
  );
}
export function useI18n() {
  const snapshot = useSyncExternalStore(subscribeLanguage, getLanguageSnapshot, getLanguageSnapshot);
  return { ...snapshot, t };
}
