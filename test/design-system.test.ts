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

const tokens = readFileSync(
  new URL("../node_modules/@idy/magpie-design-system/css/tokens.css", import.meta.url),
  "utf8",
);
const components = readFileSync(
  new URL("../node_modules/@idy/magpie-design-system/css/components.css", import.meta.url),
  "utf8",
);
const bundle = { version: bundledDesignSystemVersion, tokens, components };
const mockFetch = (handler: (url: string) => Response | Promise<Response>) =>
  ((input: RequestInfo | URL) => Promise.resolve(handler(String(input)))) as typeof fetch;

test("actual release styles support older WebViews, retain local fonts, and pin icon URLs", () => {
  const css = designSystemCss(bundle);
  assert.ok(!css.includes("@layer"));
  assert.ok(css.includes(".mds-group"));
  assert.ok(css.includes(`${designSystemCdn}@v${bundle.version}/icons/check.svg`));
  assert.ok(!css.includes("@import") && !css.includes("@font-face"));
});

test("download resolves latest once and pins both layers to that release", async () => {
  const urls: string[] = [];
  const result = await downloadDesignSystem(
    mockFetch((url) => {
      urls.push(url);
      if (url.endsWith("package.json")) return Response.json({ name: "@idy/magpie-design-system", version: "0.5.0" });
      return new Response(url.endsWith("/tokens.css") ? tokens : components);
    }),
  );
  assert.equal(result?.version, "0.5.0");
  assert.deepEqual(urls, [
    `${designSystemCdn}@latest/package.json`,
    `${designSystemCdn}@v0.5.0/css/tokens.css`,
    `${designSystemCdn}@v0.5.0/css/components.css`,
  ]);
});

test("current or stale manifests do not download immutable stylesheets again", async () => {
  for (const current of ["0.4.3", "0.5.0"]) {
    const urls: string[] = [];
    const result = await downloadDesignSystem(
      mockFetch((url) => {
        urls.push(url);
        return Response.json({ name: "@idy/magpie-design-system", version: "0.4.3" });
      }),
      undefined,
      current,
    );
    assert.equal(result, undefined);
    assert.equal(urls.length, 1);
  }
});

test("a failed layer rejects the whole update", async () => {
  await assert.rejects(
    downloadDesignSystem(
      mockFetch((url) => {
        if (url.endsWith("package.json"))
          return Response.json({ name: "@idy/magpie-design-system", version: bundle.version });
        if (url.endsWith("/components.css")) return new Response("Unavailable", { status: 503 });
        return new Response(tokens);
      }),
    ),
    /components: 503/,
  );
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

test("invalid content, old versions, previews and new majors never replace current styles", () => {
  for (const version of ["0.4.1", "0.3.99", "0.4.4-beta.1", "1.0.0", "../../main", "latest"])
    assert.throws(() => designSystemCss({ ...bundle, version }));
  for (const patch of [
    { tokens: "<html>error</html>" },
    { tokens: tokens.split("/* Dark theme, explicit.")[0] },
    { components: "" },
    { components: components + '\n@import "https://example.com/style.css";' },
    { components: components + '\n.mds-row { background: url("https://example.com/image"); }' },
    { tokens: tokens + " ".repeat(256 * 1024) },
  ])
    assert.throws(() => designSystemCss({ ...bundle, ...patch }));
});

test("last successful bundle restores offline; corrupt or inaccessible caches use bundled CSS", () => {
  assert.deepEqual(restoreDesignSystem({ getItem: () => JSON.stringify(bundle) }), bundle);
  for (const cached of [null, "{", "null", "{}", JSON.stringify({ ...bundle, version: "0.1.0" })])
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

test("native policies allow CDN styles without allowing remote scripts or fonts", () => {
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
    assert.equal(directive("font-src").trim(), "font-src 'self'");
  }
});
