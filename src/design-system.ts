// Only styles are updated remotely. React components and behavior ship with the app.
export const bundledDesignSystemVersion = "0.4.4";
export const designSystemCdn = "https://cdn.jsdelivr.net/gh/idy/magpie-design-system";
const cacheKey = "gizmoth.design-system.v2";
const refreshInterval = 6 * 60 * 60 * 1000;
const maxStylesheetLength = 256 * 1024;
const maxFontLength = 512 * 1024;
const maxFontsStylesheetLength = 2 * 1024 * 1024;

// Cached fonts contain their bytes, not network URLs, so offline restores are complete.
type StyleBundle = { version: string; tokens: string; components: string; fonts: string };
type FontFace = { family: string; weight: string; source: string };

function versionParts(version: string) {
  // A new major version needs an app-side compatibility review. Never load previews.
  if (!/^0\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(version)) throw new Error("Unsupported design system version");
  const parts = version.split(".").map(Number);
  if (!parts.every(Number.isSafeInteger)) throw new Error("Unsupported design system version");
  return parts;
}

function compareVersions(left: string, right: string) {
  const a = versionParts(left);
  const b = versionParts(right);
  return a[1] - b[1] || a[2] - b[2];
}

function fontFaces(css: string): FontFace[] {
  if (typeof css !== "string" || !css.trim() || css.length > maxFontsStylesheetLength)
    throw new Error("Invalid design system fonts stylesheet");
  const source = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const faces: FontFace[] = [];
  const remaining = source.replace(/@font-face\s*\{([^{}]*)\}/gi, (_, body: string) => {
    // A data URL contains a semicolon; extract src before splitting declarations.
    let fontSource: string | undefined;
    const descriptors = body.replace(
      /(^|;)\s*src\s*:\s*url\(\s*(["']?)([^)"']+)\2\s*\)\s+format\(["']woff2["']\)\s*(?=;|$)/gi,
      (_, separator: string, _quote: string, path: string) => {
        if (fontSource !== undefined) throw new Error("Duplicate design system font source");
        fontSource = path.trim();
        return separator;
      },
    );
    const declarations = new Map<string, string>();
    for (const declaration of descriptors.split(";").filter((part) => part.trim())) {
      const separator = declaration.indexOf(":");
      const name = declaration.slice(0, separator).trim().toLowerCase();
      if (separator < 0 || declarations.has(name)) throw new Error("Invalid design system font declaration");
      declarations.set(name, declaration.slice(separator + 1).trim());
    }
    const family = declarations.get("font-family")?.replace(/^(?:"([^"]+)"|'([^']+)')$/, "$1$2");
    const weight = declarations.get("font-weight")?.replace(/\s+/g, " ");
    const weights = weight?.split(" ").map(Number) ?? [];
    if (
      declarations.size !== 4 ||
      !family ||
      !/^[A-Za-z][A-Za-z0-9 -]{0,79}$/.test(family) ||
      !weight ||
      !/^\d{1,4}(?: \d{1,4})?$/.test(weight) ||
      !weights.length ||
      weights.length > 2 ||
      !weights.every((value) => Number.isInteger(value) && value >= 1 && value <= 1000) ||
      (weights.length === 2 && weights[0] > weights[1]) ||
      declarations.get("font-style") !== "normal" ||
      declarations.get("font-display") !== "swap" ||
      !fontSource
    )
      throw new Error("Incompatible design system font face");
    faces.push({ family, weight: weight!, source: fontSource });
    return "";
  });
  if (remaining.trim() || !faces.length) throw new Error("Unexpected design system fonts rule");
  return faces;
}

function renderFonts(faces: FontFace[]) {
  return faces
    .map(
      (face) =>
        `@font-face { font-family: "${face.family}"; font-style: normal; font-weight: ${face.weight}; font-display: swap; src: url("${face.source}") format("woff2"); }`,
    )
    .join("\n");
}

function assetUrl(path: string, version: string, directory: "icons" | "fonts") {
  const base = `${designSystemCdn}@v${version}/`;
  const url = new URL(path, `${base}css/`);
  const prefix = `${base}${directory}/`;
  const filename = directory === "fonts" ? /^[\w-]+\.woff2$/ : /^[\w-]+\.svg$/;
  if (!url.href.startsWith(prefix) || !filename.test(url.href.slice(prefix.length)))
    throw new Error(`Unexpected design system ${directory} asset`);
  return url.href;
}

function validateFont(bytes: Uint8Array) {
  if (
    bytes.length < 48 ||
    bytes.length > maxFontLength ||
    bytes[0] !== 0x77 ||
    bytes[1] !== 0x4f ||
    bytes[2] !== 0x46 ||
    bytes[3] !== 0x32
  )
    throw new Error("Invalid design system WOFF2 font");
}

function fontDataUrl(bytes: Uint8Array) {
  validateFont(bytes);
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000)
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  return `data:font/woff2;base64,${btoa(binary)}`;
}

function validateFontDataUrl(source: string) {
  const encoded = source.match(/^data:font\/woff2;base64,([A-Za-z0-9+/]+={0,2})$/)?.[1];
  if (!encoded || encoded.length > Math.ceil(maxFontLength / 3) * 4 || encoded.length % 4)
    throw new Error("Invalid cached design system font");
  validateFont(Uint8Array.from(atob(encoded), (character) => character.charCodeAt(0)));
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
  const typeTokens = [
    "font",
    "font-heading",
    "font-display",
    "font-action",
    "mono",
    "text-body",
    "text-heading",
    "text-title",
    "text-note",
    "text-caption",
    "text-code",
  ];
  if (
    themes.some((theme) => !theme || !required.every((token) => theme.includes(`--mds-${token}:`))) ||
    !typeTokens.every((token) => new RegExp(`--mds-${token}\\s*:\\s*[^;{}]+;`).test(source)) ||
    ![".mds-group", ".mds-row", ".mds-pre", ".mds-link-button"].every((selector) =>
      bundle.components.includes(selector),
    )
  )
    throw new Error("Incompatible or incomplete design system stylesheets");

  // Flatten only the expected font import; no other imports or font registration
  // can enter through the token/component layers, including CSS escape sequences.
  const tokens = source.replace(/@import\s+url\(\s*(["']?)fonts\.css\1\s*\)\s*;/gi, "");
  const components = bundle.components.replace(/\/\*[\s\S]*?\*\//g, "");
  const layers = `${tokens}\n${components}`;
  if (/@import\b|@font-face\b|\\/i.test(layers)) throw new Error("Unexpected design system import");
  const css = layers.replace(
    /url\(\s*(["']?)([^)"']+)\1\s*\)/gi,
    (_, _quote, path: string) => `url("${assetUrl(path.trim(), bundle.version, "icons")}")`,
  );
  const faces = fontFaces(bundle.fonts);
  for (const face of faces) validateFontDataUrl(face.source);
  return `${renderFonts(faces)}\n${css}`;
}

export async function downloadDesignSystem(
  fetchStyles: typeof fetch,
  signal?: AbortSignal,
  currentVersion?: string,
): Promise<StyleBundle | undefined> {
  const options: RequestInit = {
    cache: "no-cache",
    credentials: "omit",
    referrerPolicy: "no-referrer",
    redirect: "error",
    signal,
  };
  const manifest = await fetchStyles(`${designSystemCdn}@latest/package.json`, options);
  if (!manifest.ok) throw new Error(`Design system manifest: ${manifest.status}`);
  const metadata = await manifest.json();
  if (metadata.name !== "@idy/magpie-design-system" || typeof metadata.version !== "string")
    throw new Error("Invalid design system manifest");
  const version = metadata.version;
  versionParts(version);
  if (currentVersion && compareVersions(version, currentVersion) <= 0) return;
  // Pin all layers and font bytes to one release, even if latest moves mid-download.
  const [tokens, components, fonts] = await Promise.all(
    ["tokens", "components", "fonts"].map(async (layer) => {
      const response = await fetchStyles(`${designSystemCdn}@v${version}/css/${layer}.css`, options);
      if (!response.ok) throw new Error(`Design system ${layer}: ${response.status}`);
      const css = await response.text();
      if (css.length > maxStylesheetLength) throw new Error("Invalid design system stylesheet");
      return css;
    }),
  );
  const faces = fontFaces(fonts);
  // Validate every URL before fetching any font. Cached/remote data URLs are not
  // accepted here; only the exact repository and selected tag can supply assets.
  const urls = faces.map((face) => assetUrl(face.source, version, "fonts"));
  const embedded = await Promise.all(
    urls.map(async (url) => {
      const response = await fetchStyles(url, options);
      if (!response.ok) throw new Error(`Design system font: ${response.status}`);
      if (Number(response.headers.get("content-length")) > maxFontLength)
        throw new Error("Invalid design system WOFF2 font");
      return fontDataUrl(new Uint8Array(await response.arrayBuffer()));
    }),
  );
  const bundle = {
    version,
    tokens,
    components,
    fonts: renderFonts(faces.map((face, index) => ({ ...face, source: embedded[index] }))),
  };
  designSystemCss(bundle);
  return bundle;
}

export function restoreDesignSystem(storage: Pick<Storage, "getItem">): StyleBundle | undefined {
  try {
    const cached = storage.getItem(cacheKey);
    if (!cached || cached.length > (maxStylesheetLength * 2 + maxFontsStylesheetLength) * 2) return;
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
