import { useSyncExternalStore } from "react";
import { getThemeSnapshot, subscribeTheme } from "./theme";

export function useTheme() {
  return useSyncExternalStore(subscribeTheme, getThemeSnapshot, getThemeSnapshot);
}
