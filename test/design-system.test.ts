import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import {
  bundledDesignSystemVersion,
  designSystemCdn,
  designSystemCss,
  downloadDesignSystem,
  restoreDesignSystem,
} from "../src/design-system.ts";

const packageRoot = new URL("../node_modules/@idy/magpie-design-system/", import.meta.url);
const tokens = readFileSync(new URL("css/tokens.css", packageRoot), "utf8");
const components = readFileSync(new URL("css/components.css", packageRoot), "utf8");
const fonts = readFileSync(new URL("css/fonts.css", packageRoot), "utf8");
const fontAssets = new Map(
  ["outfit-latin.woff2", "source-sans-3-latin.woff2"].map((name) => [
    name,
    readFileSync(new URL(`fonts/${name}`, packageRoot)),
  ]),
);
const embeddedFonts = fonts.replace(
  /url\(["']\.\.\/fonts\/([\w-]+\.woff2)["']\)/g,
  (_, name: string) => `url("data:font/woff2;base64,${fontAssets.get(name)!.toString("base64")}")`,
);
const bundle = { version: bundledDesignSystemVersion, tokens, components, fonts: embeddedFonts };
const nextVersion = "0.4.5";
const mockFetch = (handler: (url: string, options?: RequestInit) => Response | Promise<Response>) =>
  ((input: RequestInfo | URL, options?: RequestInit) =>
    Promise.resolve(handler(String(input), options))) as typeof fetch;
const serve = (url: string, version = nextVersion, fontCss = fonts): Response => {
  if (url === `${designSystemCdn}@latest/package.json`)
    return Response.json({ name: "@idy/magpie-design-system", version });
  const base = `${designSystemCdn}@v${version}/`;
  for (const [layer, css] of Object.entries({ tokens, components, fonts: fontCss }))
    if (url === `${base}css/${layer}.css`) return new Response(css);
  for (const [name, bytes] of fontAssets) if (url === `${base}fonts/${name}`) return new Response(bytes);
  throw new Error(`Unexpected fixture URL: ${url}`);
};

test("actual 0.4.4 styles retain title/action light and regular/bold faces offline, and pin icons", () => {
  assert.equal(JSON.parse(readFileSync(new URL("package.json", packageRoot), "utf8")).version, "0.4.4");
  const css = designSystemCss(bundle);
  assert.ok(!css.includes("@layer") && !css.includes("@import"));
  assert.ok(css.includes(".mds-group"));
  assert.ok(css.includes(`${designSystemCdn}@v${bundle.version}/icons/check.svg`));
  assert.match(css, /font-family: "Outfit"; font-style: normal; font-weight: 300;/);
  assert.match(css, /font-family: "Source Sans 3"; font-style: normal; font-weight: 400 700;/);
  assert.equal((css.match(/data:font\/woff2;base64,/g) ?? []).length, 2);
  assert.ok(!css.includes("../fonts/") && !css.includes(`${designSystemCdn}@v${bundle.version}/fonts/`));
});

test("download pins all layers and WOFF2 bytes to one release before returning a cacheable bundle", async () => {
  const urls: string[] = [];
  const controller = new AbortController();
  const result = await downloadDesignSystem(
    mockFetch((url, options) => {
      urls.push(url);
      assert.equal(options?.credentials, "omit");
      assert.equal(options?.referrerPolicy, "no-referrer");
      assert.equal(options?.redirect, "error");
      assert.equal(options?.signal, controller.signal);
      return serve(url);
    }),
    controller.signal,
  );
  assert.equal(result?.version, nextVersion);
  assert.deepEqual(urls, [
    `${designSystemCdn}@latest/package.json`,
    `${designSystemCdn}@v${nextVersion}/css/tokens.css`,
    `${designSystemCdn}@v${nextVersion}/css/components.css`,
    `${designSystemCdn}@v${nextVersion}/css/fonts.css`,
    ...[...fontAssets.keys()].map((name) => `${designSystemCdn}@v${nextVersion}/fonts/${name}`),
  ]);
  const embedded = [...result!.fonts.matchAll(/data:font\/woff2;base64,([A-Za-z0-9+/=]+)/g)];
  assert.equal(embedded.length, fontAssets.size);
  for (const [index, bytes] of [...fontAssets.values()].entries())
    assert.deepEqual(Buffer.from(embedded[index][1], "base64"), bytes);
  assert.ok(designSystemCss(result!).includes("data:font/woff2;base64,"));
});

test("current or stale manifests do not download immutable stylesheets or fonts again", async () => {
  for (const current of [bundledDesignSystemVersion, nextVersion]) {
    const urls: string[] = [];
    const result = await downloadDesignSystem(
      mockFetch((url) => {
        urls.push(url);
        return serve(url, bundledDesignSystemVersion);
      }),
      undefined,
      current,
    );
    assert.equal(result, undefined);
    assert.equal(urls.length, 1);
  }
});

test("a failed component or fonts layer rejects the whole update", async () => {
  for (const layer of ["components", "fonts"]) {
    await assert.rejects(
      downloadDesignSystem(
        mockFetch((url) =>
          url.endsWith(`/css/${layer}.css`) ? new Response("Unavailable", { status: 503 }) : serve(url),
        ),
      ),
      new RegExp(`${layer}: 503`),
    );
  }
});

test("a failed font download cannot return or cache a partial bundle", async () => {
  for (const filename of fontAssets.keys()) {
    let result;
    await assert.rejects(async () => {
      result = await downloadDesignSystem(
        mockFetch((url) =>
          url.endsWith(`/fonts/${filename}`) ? new Response("Unavailable", { status: 503 }) : serve(url),
        ),
      );
    }, /font: 503/);
    assert.equal(result, undefined);
  }
});

test("font bytes must have a WOFF2 signature and stay within the download size limit", async () => {
  for (const response of [
    () => new Response("<html>not a font</html>"),
    () => new Response(new Uint8Array(64)),
    () => new Response(new Uint8Array(512 * 1024 + 1)),
    () => new Response("", { headers: { "content-length": String(512 * 1024 + 1) } }),
  ]) {
    await assert.rejects(
      downloadDesignSystem(mockFetch((url) => (url.endsWith("/outfit-latin.woff2") ? response() : serve(url)))),
      /WOFF2/,
    );
  }
});

test("unexpected font URLs are rejected before any font is fetched", async () => {
  for (const source of [
    "https://example.com/font.woff2",
    "//example.com/font.woff2",
    `${designSystemCdn}@v0.4.3/fonts/outfit-latin.woff2`,
    "https://cdn.jsdelivr.net/gh/idy/other@v0.4.5/fonts/outfit-latin.woff2",
    "../fonts/../../other/font.woff2",
    "../icons/outfit-latin.woff2",
    "../fonts/outfit.ttf",
    "../fonts/outfit-latin.woff2?tracking=1",
    "../fonts/outfit-latin.woff2#fragment",
    "data:font/woff2;base64,d09GMg==",
  ]) {
    const urls: string[] = [];
    const fontCss = fonts.replace("../fonts/source-sans-3-latin.woff2", source);
    await assert.rejects(
      downloadDesignSystem(
        mockFetch((url) => {
          urls.push(url);
          return serve(url, nextVersion, fontCss);
        }),
      ),
      /Unexpected design system fonts asset/,
    );
    assert.equal(urls.length, 4, `Fetched a font before validating ${source}`);
  }
});

test("compatible future font families and additional faces can update without an app release", async () => {
  const futureFonts =
    fonts.replace("Outfit", "Future Display").replace("Source Sans 3", "Future Text") +
    '\n@font-face { font-family: "Future Alternate"; font-style: normal; font-weight: 500; font-display: swap; src: url("../fonts/outfit-latin.woff2") format("woff2"); }';
  const result = await downloadDesignSystem(
    mockFetch((url) => {
      if (url.endsWith("/tokens.css"))
        return new Response(tokens.replaceAll("Outfit", "Future Display").replaceAll("Source Sans 3", "Future Text"));
      return serve(url, nextVersion, futureFonts);
    }),
  );
  const css = designSystemCss(result!);
  assert.match(css, /font-family: "Future Display"/);
  assert.match(css, /font-family: "Future Text"/);
  assert.match(css, /font-family: "Future Alternate"/);
});

test("offline and cancelled requests reject without creating an update", async () => {
  await assert.rejects(
    downloadDesignSystem(
      mockFetch(() => {
        throw new Error("offline");
      }),
    ),
    /offline/,
  );
  const controller = new AbortController();
  controller.abort();
  const fetchCancelled = (async (_input: RequestInfo | URL, options?: RequestInit) => {
    options?.signal?.throwIfAborted();
    return new Response();
  }) as typeof fetch;
  await assert.rejects(downloadDesignSystem(fetchCancelled, controller.signal), { name: "AbortError" });
});

test("invalid styles, imports, missing type roles, previews and new majors cannot replace current styles", () => {
  for (const version of ["0.4.3", "0.3.99", "0.4.5-beta.1", "0.04.5", "1.0.0", "../../main", "latest"])
    assert.throws(() => designSystemCss({ ...bundle, version }));
  for (const patch of [
    { tokens: "<html>error</html>" },
    { tokens: tokens.split("/* Dark theme, explicit.")[0] },
    { tokens: tokens.replace("--mds-font-action:", "--missing-font-action:") },
    { tokens: tokens + '\n@font-face { font-family: injected; src: url("../icons/check.svg"); }' },
    { tokens: tokens + '\n@import "fonts.css";' },
    { components: "" },
    { components: components + '\n@import "https://example.com/style.css";' },
    { components: components + "\n@font-face { font-family: injected; }" },
    { components: components + '\n.mds-row { background: url("https://example.com/image"); }' },
    { components: components + '\n.mds-row { background: u\\72l("https://example.com/image"); }' },
    { components: components + `\n.mds-row { background: url("${designSystemCdn}@v0.4.3/icons/check.svg"); }` },
    { tokens: tokens + " ".repeat(256 * 1024) },
  ])
    assert.throws(() => designSystemCss({ ...bundle, ...patch }));
});

test("fonts CSS accepts only safe font declarations with embedded WOFF2 data", () => {
  for (const invalid of [
    "",
    fonts,
    embeddedFonts + '\n@import "fonts.css";',
    embeddedFonts + "\nbody { color: red; }",
    embeddedFonts.replace("font-display: swap", "font-display: block"),
    embeddedFonts.replace("font-weight: 300", "font-weight: 0"),
    embeddedFonts.replace("font-weight: 400 700", "font-weight: 700 400"),
    embeddedFonts.replace("font-style: normal", "font-style: normal; color: red"),
    embeddedFonts.replace("font-weight: 300;", "font-weight: 300; font-weight: 700;"),
    embeddedFonts.replace(/data:font\/woff2;base64,[A-Za-z0-9+/=]+/, "data:font/woff2;base64,d09GMg=="),
    embeddedFonts.replace(/data:font\/woff2;base64,[A-Za-z0-9+/=]+/, "data:text/html;base64,PHNjcmlwdD4="),
  ])
    assert.throws(() => designSystemCss({ ...bundle, fonts: invalid }));
});

test("v2 cache restores the downloaded font bytes offline and rejects legacy or corrupt caches", async () => {
  const downloaded = await downloadDesignSystem(mockFetch((url) => serve(url)));
  let key;
  const restored = restoreDesignSystem({
    getItem: (name) => {
      key = name;
      return JSON.stringify(downloaded);
    },
  });
  assert.equal(key, "gizmoth.design-system.v2");
  assert.deepEqual(restored, downloaded);
  assert.ok(designSystemCss(restored!).includes("data:font/woff2;base64,"));
  for (const cached of [
    null,
    "{",
    "null",
    "{}",
    JSON.stringify({ ...bundle, version: "0.1.0" }),
    JSON.stringify({ version: bundle.version, tokens, components }),
    JSON.stringify({ ...bundle, fonts }),
  ])
    assert.equal(restoreDesignSystem({ getItem: () => cached }), undefined);
  assert.equal(
    restoreDesignSystem({
      getItem: () => {
        throw new Error("Storage disabled");
      },
    }),
    undefined,
  );
});

test("native policies allow CDN styles and embedded fonts without allowing remote scripts or fonts", () => {
  const main = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8")).app.security
    .csp;
  const simulator = readFileSync(new URL("../src-tauri/src/simulator.rs", import.meta.url), "utf8").match(
    /SIMULATOR_CSP: &str = "([^"]+)"/,
  )![1];
  for (const policy of [main, simulator]) {
    const directive = (name: string) => policy.split(";").find((part: string) => part.trim().startsWith(name + " "));
    assert.ok(directive("connect-src").includes("https://cdn.jsdelivr.net/gh/idy/"));
    assert.ok(directive("img-src").includes("https://cdn.jsdelivr.net/gh/idy/"));
    assert.ok(!directive("script-src").includes("jsdelivr"));
    assert.deepEqual(directive("font-src").trim().split(/\s+/), ["font-src", "'self'", "data:"]);
  }
});
