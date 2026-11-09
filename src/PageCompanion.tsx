import { useEffect } from "react";
import type { Peetty } from "@idy/peetty";
import { setCompanionEnabled, useCompanionEnabled } from "./companion-preferences";
import { useI18n } from "./i18n";
import companionStyles from "./PageCompanion.css?inline";

/** The pinned SDK isolates its UI in a shadow root; adapt that surface to the app. */
function adaptCompanion(language: "en" | "zh-CN") {
  const host = document.querySelector("peetty-companion");
  const root = host?.shadowRoot;
  const figure = root?.querySelector<HTMLElement>(".pet");
  const menu = root?.querySelector<HTMLElement>(".menu");
  if (!root || !figure || !menu) return;

  const style = document.createElement("style");
  style.textContent = companionStyles;
  root.append(style);
  figure.tabIndex = 0;
  figure.setAttribute("role", "button");
  figure.setAttribute("aria-label", language === "en" ? "Page companion: open actions" : "页面小人：打开互动菜单");
  figure.setAttribute("aria-haspopup", "true");
  figure.addEventListener("keydown", (event) => {
    if (event.isComposing || (event.key !== "Enter" && event.key !== " ")) return;
    event.preventDefault();
    event.stopPropagation();
    menu.classList.add("show");
    menu.querySelector("button")?.focus();
  });
  menu.addEventListener("keydown", (event) => {
    if (event.isComposing || event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    figure.focus();
    menu.classList.remove("show");
  });
  // Keep the SDK's Hide action and the app's persistent switch in agreement.
  menu.querySelector("button:last-child")?.addEventListener("click", () => setCompanionEnabled(false));
}

export function PageCompanion() {
  const enabled = useCompanionEnabled();
  const { language } = useI18n();

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let companion: Peetty | undefined;
    const syncVisibility = () => {
      if (document.hidden) companion?.hide();
      else companion?.show();
    };
    document.addEventListener("visibilitychange", syncVisibility);
    // Load after the shell, and retain ownership before async mounting so StrictMode
    // cleanup also invalidates an unfinished mount.
    void import("@idy/peetty")
      .then(async ({ createPeetty }) => {
        if (disposed) return;
        companion = createPeetty({
          figure: "miru",
          size: 92,
          activity: "lively",
          language: language === "zh-CN" ? "zh" : "en",
          greet: false,
          zIndex: 20,
        });
        await companion.mount();
        if (disposed) return;
        adaptCompanion(language);
        syncVisibility();
      })
      .catch((error: unknown) => {
        companion?.destroy();
        if (!disposed) console.warn("Could not start the page companion", error);
      });
    return () => {
      disposed = true;
      document.removeEventListener("visibilitychange", syncVisibility);
      companion?.destroy();
    };
  }, [enabled, language]);

  return null;
}
