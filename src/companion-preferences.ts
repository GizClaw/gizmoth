import { useSyncExternalStore } from "react";

const storageKey = "gizmoth.companion";
const changeEvent = "gizmoth-companion-change";

function subscribe(listener: () => void) {
  const onStorage = (event: StorageEvent) => {
    if (event.key === storageKey || event.key === null) listener();
  };
  window.addEventListener(changeEvent, listener);
  window.addEventListener("storage", onStorage);
  return () => {
    window.removeEventListener(changeEvent, listener);
    window.removeEventListener("storage", onStorage);
  };
}

export function useCompanionEnabled() {
  return useSyncExternalStore(subscribe, () => localStorage.getItem(storageKey) !== "off");
}

export function setCompanionEnabled(enabled: boolean) {
  localStorage.setItem(storageKey, enabled ? "on" : "off");
  window.dispatchEvent(new Event(changeEvent));
}
