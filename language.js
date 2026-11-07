// Preserve the existing Chinese URL while using the shared language controls.
(() => {
  const requested = new URLSearchParams(location.search).get("lang");
  const language = ["en", "zh"].includes(requested) ? requested
    : location.pathname.endsWith("/zh-cn.html") ? "zh" : null;
  if (!language) return;
  try { localStorage.setItem("idy-language", language); } catch { /* Apply the preference in memory after the shared controls initialize. */ }
  window.addEventListener("load", () => {
    const select = document.querySelector("[data-idy-language]");
    if (select && select.value !== language) {
      select.value = language;
      select.dispatchEvent(new Event("change", { bubbles: true }));
    }
  });
})();
