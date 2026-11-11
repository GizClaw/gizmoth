// Only styles are updated remotely. React components and behavior ship with the app.
export const bundledDesignSystemVersion = "0.4.3";
export const designSystemCdn = "https://cdn.jsdelivr.net/gh/idy/magpie-design-system";
const cacheKey = "gizmoth.design-system.v1";
const refreshInterval = 6 * 60 * 60 * 1000;
const maxStylesheetLength = 256 * 1024;

type StyleBundle = { version: string; tokens: string; components: string };

function versionParts(version: string) {
  // A new major version needs an app-side compatibility review. Never load previews.
  if (!/^0\.\d+\.\d+$/.test(version)) throw new Error("Unsupported design system version");
  return version.split(".").map(Number);
}

function compareVersions(left: string, right: string) {
  const a = versionParts(left);
  const b = versionParts(right);
  return a[1] - b[1] || a[2] - b[2];
}

export function designSystemCss(bundle: StyleBundle): string {
  if (compareVersions(bundle.version, bundledDesignSystemVersion) < 0)
    throw new Error("Design system is older than the bundled fallback");
  for (const css of [bundle.tokens, bundle.components]) {
    if (typeof css !== "string" || !css.trim() || css.length > maxStylesheetLength || /<\/?(?:html|script)/i.test(css))
      throw new Error("Invalid design system stylesheet");
  }
  const source = bundle.tokens.replace(/\/\*[\s\S]*?\*\//g, "");
  const themes = [
    source.match(/\[data-theme=["']light["']\]\s*\{([^}]+)\}/)?.[1],
    source.match(/\[data-theme=["']dark["']\]\s*\{([^}]+)\}/)?.[1],
    source.match(/:root:not\(\[data-theme=["']light["']\]\)\s*\{([^}]+)\}/)?.[1],
  ];
  const required = ["canvas", "sidebar", "surface", "line", "ink", "muted", "accent", "on-accent"];
  if (
    themes.some((theme) => !theme || !required.every((token) => theme.includes(`--mds-${token}:`))) ||
    ![".mds-group", ".mds-row", ".mds-pre", ".mds-link-button"].every((selector) =>
      bundle.components.includes(selector),
    )
  )
    throw new Error("Incompatible or incomplete design system stylesheets");

  // GizMoth keeps its bundled fonts and regular-only typography.
  const tokens = bundle.tokens.replace(/@import\s+url\(["']?fonts\.css["']?\)\s*;/g, "");
  const base = `${designSystemCdn}@v${bundle.version}/`;
  const css = `${tokens}\n${bundle.components}`.replace(
    /url\(\s*(["']?)([^)"']+)\1\s*\)/g,
    (_, _quote, path: string) => {
      const url = new URL(path.trim(), `${base}css/`);
      if (!url.href.startsWith(`${base}icons/`)) throw new Error("Unexpected design system asset");
      return `url("${url.href}")`;
    },
  );
  if (/@import\b|@font-face\b/i.test(css)) throw new Error("Unexpected design system import");
  return css;
}

export async function downloadDesignSystem(
  fetchStyles: typeof fetch,
  signal?: AbortSignal,
  currentVersion?: string,
): Promise<StyleBundle | undefined> {
  const options: RequestInit = { cache: "no-cache", credentials: "omit", referrerPolicy: "no-referrer", signal };
  const manifest = await fetchStyles(`${designSystemCdn}@latest/package.json`, options);
  if (!manifest.ok) throw new Error(`Design system manifest: ${manifest.status}`);
  const metadata = await manifest.json();
  if (metadata.name !== "@idy/magpie-design-system" || typeof metadata.version !== "string")
    throw new Error("Invalid design system manifest");
  const version = metadata.version;
  versionParts(version);
  if (currentVersion && compareVersions(version, currentVersion) <= 0) return;
  // Pin both layers and all their assets to one release, even if latest moves mid-download.
  const [tokens, components] = await Promise.all(
    ["tokens", "components"].map(async (layer) => {
      const response = await fetchStyles(`${designSystemCdn}@v${version}/css/${layer}.css`, options);
      if (!response.ok) throw new Error(`Design system ${layer}: ${response.status}`);
      return response.text();
    }),
  );
  const bundle = { version, tokens, components };
  designSystemCss(bundle);
  return bundle;
}

export function restoreDesignSystem(storage: Pick<Storage, "getItem">): StyleBundle | undefined {
  try {
    const cached = storage.getItem(cacheKey);
    if (!cached || cached.length > maxStylesheetLength * 3) return;
    const bundle: StyleBundle = JSON.parse(cached);
    designSystemCss(bundle);
    return bundle;
  } catch {
    // Corrupt, old, or unavailable storage must never prevent startup.
    return;
  }
}

export function startDesignSystemUpdates(bundledStyles: string) {
  // Capture native WebView fetch before provider requests are routed through Tauri.
  const fetchStyles = window.fetch.bind(window);
  let currentVersion = bundledDesignSystemVersion;
  const style = document.getElementById("gizmoth-design-system") ?? document.createElement("style");
  style.id = "gizmoth-design-system";
  style.dataset.version = currentVersion;
  style.dataset.source = "bundled";
  style.textContent = bundledStyles;
  // App styles always follow this node. Replacing it also removes deleted upstream rules.
  document.head.prepend(style);
  const apply = (bundle: StyleBundle, source: "cache" | "cdn") => {
    if (compareVersions(bundle.version, currentVersion) < 0) return;
    const css = designSystemCss(bundle);
    style.textContent = css;
    style.dataset.version = bundle.version;
    style.dataset.source = source;
    currentVersion = bundle.version;
  };
  try {
    const cached = restoreDesignSystem(window.localStorage);
    if (cached) apply(cached, "cache");
  } catch {
    /* Storage access itself can be disabled. Use bundled CSS. */
  }

  let checking = false;
  let retryRequested = false;
  let disposed = false;
  let activeRequest: AbortController | undefined;
  const check = async () => {
    if (disposed) return;
    if (checking) {
      retryRequested = true;
      return;
    }
    checking = true;
    const controller = new AbortController();
    activeRequest = controller;
    // AbortSignal.timeout is unavailable on some supported macOS 12 WebViews.
    const timeout = window.setTimeout(() => controller.abort(), 15_000);
    try {
      const bundle = await downloadDesignSystem(fetchStyles, controller.signal, currentVersion);
      if (disposed || !bundle) return;
      apply(bundle, "cdn");
      try {
        localStorage.setItem(cacheKey, JSON.stringify(bundle));
      } catch {
        /* Quota/private mode. */
      }
    } catch (error) {
      if (!disposed) console.info("Design system update unavailable; keeping current styles.", error);
    } finally {
      window.clearTimeout(timeout);
      activeRequest = undefined;
      checking = false;
      if (!disposed && retryRequested) {
        retryRequested = false;
        void check();
      }
    }
  };
  void check();
  const interval = window.setInterval(() => void check(), refreshInterval);
  window.addEventListener("online", check);
  return () => {
    disposed = true;
    activeRequest?.abort();
    window.clearInterval(interval);
    window.removeEventListener("online", check);
  };
}
