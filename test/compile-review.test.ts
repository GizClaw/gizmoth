import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import ts from "typescript";

type Element = { type: unknown; props: Record<string, any> };
const jsx = (type: unknown, props: Element["props"]): Element => ({ type, props });
const settle = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
function deferred<T = any>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

// Run real component effects with controlled I/O, without a DOM or timing sleeps.
function hooks() {
  const slots: any[] = [];
  let cursor = 0;
  let effects: (() => void)[] = [];
  const react = {
    useRef(value: unknown) { const index = cursor++; return slots[index] ??= { current: value }; },
    useState(value: any) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = typeof value === "function" ? value() : value;
      return [slots[index], (next: any) => { slots[index] = typeof next === "function" ? next(slots[index]) : next; }];
    },
    useEffect(effect: () => void | (() => void), deps: unknown[]) {
      const index = cursor++;
      const before = slots[index];
      if (before && deps.every((value, i) => Object.is(value, before.deps[i]))) return;
      effects.push(() => { before?.cleanup?.(); slots[index] = { deps, cleanup: effect() }; });
    },
    useMemo(factory: () => unknown, deps: unknown[]) {
      const index = cursor++;
      const before = slots[index];
      if (!before || !deps.every((value, i) => Object.is(value, before.deps[i])))
        slots[index] = { deps, value: factory() };
      return slots[index].value;
    },
    useCallback(callback: unknown, deps: unknown[]) { return react.useMemo(() => callback, deps); },
    useSyncExternalStore(_subscribe: unknown, read: () => unknown) { return read(); },
    useId() { return "field"; },
    memo(component: unknown) { return component; },
  };
  return {
    react,
    render<T>(component: () => T) { cursor = 0; return component(); },
    flush() { const pending = effects; effects = []; for (const effect of pending) effect(); },
    unmount() { for (const slot of slots) slot?.cleanup?.(); },
  };
}
function load(file: string, imports: Record<string, unknown>, expose = "") {
  const source = readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    fileName: file,
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  });
  const exports: Record<string, any> = {};
  runInNewContext(outputText + expose, {
    exports, setTimeout, clearTimeout, URLSearchParams, console,
    ResizeObserver: class { observe() {} disconnect() {} },
    document: { getElementById: () => null },
    require: (name: string) => {
      if (name === "react/jsx-runtime") return { jsx, jsxs: jsx, Fragment: "fragment" };
      if (name in imports) return imports[name];
      if (name.endsWith(".css")) return {};
      throw new Error(`Unmocked dependency ${name}`);
    },
  }, { filename: file });
  return exports;
}
function elements(tree: any): Element[] {
  if (Array.isArray(tree)) return tree.flatMap(elements);
  if (!tree || typeof tree !== "object" || !("props" in tree)) return [];
  return [tree, ...elements(tree.props.children)];
}
function compiling(h: ReturnType<typeof hooks>, compile: Record<string, unknown>) {
  const ui = Object.fromEntries(["Button", "Dialog", "Dot", "IconButton", "Input", "PageHeader", "SearchInput", "Select"].map((name) => [name, name]));
  return load("pages/Compiling.tsx", {
    react: h.react, "@phosphor-icons/react": {},
    "../bridge": { errorMessage: String }, "../clipboard": {}, "./CompileTerminal": {}, "./CompileTaskConfig": {},
    "../compile": { compile, formatSize: String },
    "../i18n": { useI18n: () => ({ t: (text: string) => text }) }, "../serial": {},
    "../ui": { ...ui, relativeTime: () => "now" }, "./SettingsRow": {}, "./CompilePackages": {},
    "./WorkspaceActions": { WorkspaceActions: "WorkspaceActions" },
  }, "\nexports.ArtifactsTab = ArtifactsTab; exports.CacheDialog = CacheDialog;");
}
const state = (version: number) => ({ version, queue: [], slots: [], artifacts: { count: 0 } });

test("compile polling discards pre-mutation replies and accepts a service restart otherwise", async () => {
  const h = hooks();
  const requests: { command: string; args: any; result: ReturnType<typeof deferred> }[] = [];
  const api = load("compile.ts", {
    react: h.react,
    "./bridge": {
      native: true, errorMessage: String,
      call: (command: string, args: unknown) => {
        const result = deferred(); requests.push({ command, args, result }); return result.promise;
      },
    },
  });
  const read = () => h.render(() => api.useCompileSnapshot());
  read(); h.flush();
  assert.equal(requests[0].command, "compile_request");
  api.setCompileSnapshot(state(11));
  requests[0].result.resolve(state(10));
  await settle();
  assert.equal(read().snapshot.version, 11);
  assert.equal(requests[1].args.since, 11);
  api.setCompileSnapshot(state(12));
  requests[1].result.resolve(state(11));
  await settle();
  assert.equal(read().snapshot.version, 12);
  assert.equal(requests[2].args.since, 12);
  requests[2].result.resolve(state(1));
  await settle();
  assert.equal(read().snapshot.version, 1);
  assert.equal(requests[3].args.since, 1);
  h.unmount();
  requests[3].result.resolve(state(2));
  await settle();
  assert.equal(read().snapshot.version, 1);
});

test("artifact pin/delete reloads survive out-of-order successes, failures and unmount", async () => {
  const h = hooks();
  const reads: ReturnType<typeof deferred>[] = [];
  const pin = deferred(), remove = deferred();
  const api = compiling(h, {
    artifacts: () => { const pending = deferred(); reads.push(pending); return pending.promise; },
    pinArtifact: () => pin.promise, deleteArtifact: () => remove.promise,
  });
  const artifact = { id: "artifact", title: "Build", pinned: false, bytes: 10, createdAt: 1, files: [{ path: "firmware.bin" }] };
  let confirmation: any;
  const snapshot = { artifacts: { count: 1, bytes: 10 }, settings: { artifacts: { keepLast: 10, maxGib: 10 } } };
  const render = () => {
    const tree = h.render(() => api.ArtifactsTab({
      snapshot, highlight: "artifact", run: (operation: () => Promise<unknown>) => operation(),
      confirm: (value: unknown) => { confirmation = value; },
    }));
    h.flush();
    return elements(tree);
  };
  // Avoid the browser scroll effect while inspecting the rendered component tree.
  snapshot.artifacts.bytes = 11;
  render();
  reads[0].resolve([artifact]);
  await settle();
  let tree = render();
  snapshot.artifacts.bytes++;
  tree = render(); // A pre-pin GET is still in flight.
  const menu = () => tree.find((element) => element.type === "WorkspaceActions")!.props.actions as any[];
  const action = (label: string) => menu().find((item) => item.label === label)!;
  const details = () => tree.find((element) => (element.type as Function)?.name === "ArtifactDetails")!.props;
  action("固定").onSelect();
  reads[1].resolve([artifact]);
  await settle();
  pin.resolve({ ...artifact, pinned: true });
  await settle();
  reads[2].resolve([{ ...artifact, pinned: true }]);
  await settle();
  tree = render();
  assert.equal(details().artifact.pinned, true);
  assert.deepEqual(Array.from(menu(), (item) => item.label), ["安装", "取消固定", "删除"]);

  snapshot.artifacts.bytes++;
  tree = render(); // A pre-delete GET will finish after the mutation reload.
  action("删除").onSelect();
  const deleting = confirmation.action();
  remove.resolve({ deleted: artifact.id });
  await settle();
  reads[4].resolve([]);
  await deleting;
  reads[3].resolve([artifact]);
  await settle();
  tree = render();
  assert.equal(tree.filter((element) => element.props.className === "compile-artifact-row").length, 0);

  snapshot.artifacts.bytes++;
  render();
  snapshot.artifacts.bytes++;
  render();
  reads[6].resolve([]);
  await settle();
  reads[5].reject(new Error("stale failure"));
  await settle();
  tree = render();
  assert.equal(tree.some((element) => element.props.role === "alert"), false);
  snapshot.artifacts.bytes++;
  render();
  h.unmount();
  reads[7].resolve([artifact]);
  await settle();
});

test("artifact views follow external pin revisions and restarted stores without reloading for log updates", async () => {
  const h = hooks();
  const reads: ReturnType<typeof deferred>[] = [];
  const api = compiling(h, {
    artifacts: () => { const pending = deferred(); reads.push(pending); return pending.promise; },
  });
  const artifact = { id: "artifact", title: "Build", pinned: false, bytes: 10, createdAt: 1, files: [{ path: "firmware.bin" }] };
  const snapshot = {
    version: 100,
    artifacts: { count: 1, bytes: 10, dir: "/artifacts", revision: "store-A:1" },
    settings: { artifacts: { keepLast: 10, maxGib: 10 } },
  };
  const render = () => {
    const tree = h.render(() => api.ArtifactsTab({ snapshot, highlight: "artifact" }));
    h.flush();
    return elements(tree).find((element) => (element.type as Function)?.name === "ArtifactDetails")?.props.artifact;
  };
  render();
  reads[0].resolve([artifact]);
  await settle();
  assert.equal(render().pinned, false);
  for (let i = 0; i < 5; i++) { snapshot.version++; render(); }
  assert.equal(reads.length, 1, "job/log notifications must not reload the artifact list");

  // Another client pins/unpins the same files; totals never change.
  for (const [revision, pinned] of [["store-A:2", true], ["store-A:3", false]] as const) {
    snapshot.artifacts.revision = revision;
    render();
    reads.at(-1)!.resolve([{ ...artifact, pinned }]);
    await settle();
    assert.equal(render().pinned, pinned);
    assert.equal(snapshot.artifacts.count, 1);
    assert.equal(snapshot.artifacts.bytes, 10);
  }
  assert.equal(reads.length, 3);

  // A restarted service can reuse the counter and totals with different pins.
  snapshot.version = 1;
  snapshot.artifacts.revision = "store-B:1";
  render();
  assert.equal(reads.length, 4);
  reads[3].resolve([{ ...artifact, pinned: true }]);
  await settle();
  assert.equal(render().pinned, true);
  h.unmount();
});

test("artifact rows expand inline and keep each row's menu scoped to its own artifact", async () => {
  const h = hooks();
  const reads: ReturnType<typeof deferred>[] = [];
  const pins: [string, boolean][] = [];
  const deletes: string[] = [];
  const reveals: string[] = [];
  const api = compiling(h, {
    artifacts: () => { const pending = deferred(); reads.push(pending); return pending.promise; },
    pinArtifact: async (id: string, pinned: boolean) => { pins.push([id, pinned]); },
    deleteArtifact: async (id: string) => { deletes.push(id); },
    reveal: async (path: string) => { reveals.push(path); },
  });
  const firmware = {
    id: "firmware", title: "Firmware", pinned: true, bytes: 10, createdAt: 2, dir: "/a/firmware/files",
    client: "codex", workspace: "/w", command: ["bazel", "build"],
    files: [{ path: "app/firmware.update.tar.zlib", size: 10, absolutePath: "/a/firmware/files/app/firmware.update.tar.zlib" }],
  };
  const docs = {
    id: "docs", title: "Docs", pinned: false, bytes: 5, createdAt: 1, dir: "/a/docs/files",
    client: "codex", workspace: "/w", command: ["make"],
    files: [{ path: "out/protocol.pdf", size: 5, absolutePath: "/a/docs/files/out/protocol.pdf" }],
  };
  let confirmation: any;
  const snapshot = { artifacts: { count: 2, bytes: 15, dir: "/a", revision: "r1" }, settings: { artifacts: { keepLast: 10, maxGib: 10 } } };
  const render = () => {
    const tree = h.render(() => api.ArtifactsTab({
      snapshot, highlight: "", run: (operation: () => Promise<unknown>) => operation(),
      confirm: (value: unknown) => { confirmation = value; },
    }));
    h.flush();
    return elements(tree);
  };
  render();
  reads[0].resolve([firmware, docs]);
  await settle();
  let tree = render();
  const items = () => tree.filter((element) => element.type === "li");
  const part = (item: Element, className: string) =>
    elements(item.props.children).find((element) => element.props.className === className)!;
  const menuOf = (item: Element) =>
    elements(item.props.children).find((element) => element.type === "WorkspaceActions")!.props;
  const details = () => tree.filter((element) => (element.type as Function)?.name === "ArtifactDetails");

  // Collapsed rows: shared column cells, no dialog, and the menu is not inside the disclosure button.
  assert.equal(items().length, 2);
  for (const item of items()) {
    const row = part(item, "compile-artifact-row");
    assert.equal(row.type, "button");
    assert.equal(row.props["aria-expanded"], false);
    assert.equal(part(item, `compile-artifact-details`).props.hidden, true);
    assert.equal(part(item, "compile-artifact-details").props.id, row.props["aria-controls"]);
    for (const cell of ["compile-artifact-name", "compile-artifact-meta-text", "compile-artifact-time"])
      assert.ok(elements(row.props.children).some((element) => element.props.className === cell), cell);
    assert.equal(elements(row.props.children).some((element) => element.type === "WorkspaceActions"), false);
    assert.ok(elements(part(item, "compile-artifact-actions").props.children).some((element) => element.type === "WorkspaceActions"));
  }
  assert.equal(details().length, 0);
  assert.equal(tree.some((element) => element.type === "Dialog"), false);
  assert.match(part(items()[0], "compile-artifact-row").props["aria-label"], /Firmware，已固定，/);

  // Menus: install follows package availability; pin label follows the row's state.
  const [pinned, plain] = items().map(menuOf);
  assert.equal(pinned.label, "管理 {name}");
  assert.deepEqual(Array.from(pinned.actions, (item: any) => [item.label, Boolean(item.disabled)]), [["安装", false], ["取消固定", false], ["删除", false]]);
  assert.deepEqual(Array.from(plain.actions, (item: any) => [item.label, Boolean(item.disabled)]), [["安装", true], ["固定", false], ["删除", false]]);
  assert.ok(plain.actions[0].title);
  assert.equal(plain.actions[2].danger, true);

  // Clicking a row toggles only its own inline details.
  part(items()[1], "compile-artifact-row").props.onClick();
  tree = render();
  assert.equal(part(items()[1], "compile-artifact-row").props["aria-expanded"], true);
  assert.equal(part(items()[1], "compile-artifact-details").props.hidden, false);
  assert.equal(part(items()[0], "compile-artifact-row").props["aria-expanded"], false);
  assert.deepEqual(Array.from(details(), (element) => element.props.artifact.id), ["docs"]);
  assert.equal(tree.some((element) => element.type === "Dialog"), false);
  details()[0].props.onReveal();
  await settle();
  assert.deepEqual(reveals, ["/a/docs/files"]);

  // An eligible install opens the existing install flow for that artifact.
  menuOf(items()[0]).actions[0].onSelect();
  tree = render();
  const install = tree.find((element) => (element.type as Function)?.name === "InstallDialog");
  assert.equal(install?.props.artifact.id, "firmware");
  install!.props.onClose();

  // Menu actions target their row and leave expansion unchanged.
  menuOf(items()[1]).actions[1].onSelect();
  await settle();
  assert.deepEqual(pins, [["docs", true]]);
  menuOf(items()[0]).actions[2].onSelect();
  assert.equal(deletes.length, 0, "deletion waits for confirmation");
  assert.equal(confirmation.danger, true);
  assert.match(confirmation.body, /\{title\}/);
  const deleting = confirmation.action();
  await settle();
  assert.deepEqual(deletes, ["firmware"]);
  reads.at(-1)!.resolve([{ ...docs, pinned: true }]);
  await deleting;
  tree = render();
  assert.deepEqual(Array.from(items(), (item) => item.props.id), ["artifact-docs"]);
  assert.equal(part(items()[0], "compile-artifact-row").props["aria-expanded"], true);
  assert.deepEqual(Array.from(menuOf(items()[0]).actions, (item: any) => item.label), ["安装", "取消固定", "删除"]);
  part(items()[0], "compile-artifact-row").props.onClick();
  tree = render();
  assert.equal(part(items()[0], "compile-artifact-row").props["aria-expanded"], false);
  assert.equal(details().length, 0);
  h.unmount();
});

test("cache settings require a value while explicit zero still submits unlimited", async () => {
  const h = hooks();
  const patches: any[] = [];
  const api = compiling(h, { patchSettings: async (patch: unknown) => { patches.push(patch); } });
  const render = () => elements(h.render(() => api.CacheDialog({
    cache: { kind: "bazel" }, settings: { caches: { bazel: { maxGib: 4 }, ccache: { maxGib: 5 } } }, onClose() {},
  })));
  for (const empty of ["", "   "]) {
    let tree = render();
    tree.find((element) => element.type === "Input")!.props.onChange({ target: { value: empty } });
    tree = render();
    assert.equal(tree.find((element) => element.type === "Input")!.props.required, true);
    assert.equal(tree.find((element) => element.props.type === "submit")!.props.disabled, true);
    tree.find((element) => element.type === "form")!.props.onSubmit({ preventDefault() {} });
    await settle();
    assert.equal(patches.length, 0);
  }
  let tree = render();
  tree.find((element) => element.type === "Input")!.props.onChange({ target: { value: "0" } });
  tree = render();
  assert.equal(tree.find((element) => element.props.type === "submit")!.props.disabled, false);
  tree.find((element) => element.type === "form")!.props.onSubmit({ preventDefault() {} });
  await settle();
  assert.equal(patches[0].caches.bazel.maxGib, 0);
  assert.equal(patches[0].caches.ccache.maxGib, 5);
});

test("terminal serializes callbacks, coalesces replacements, and retires disposed writers", () => {
  const h = hooks();
  const views: FakeTerminal[] = [];
  class FakeTerminal {
    screen = "";
    pending: { text: string; complete: () => void }[] = [];
    events: string[] = [];
    buffer = { active: { viewportY: 0, baseY: 0, length: 0 } };
    constructor() { views.push(this); }
    loadAddon() {} open() {} dispose() {}
    onScroll() { return { dispose() {} }; }
    scrollToBottom() {} scrollToLine() {}
    reset() { assert.equal(this.pending.length, 0, "reset overtook a pending write"); this.screen = ""; this.events.push("reset"); }
    write(text: string, complete: () => void) {
      assert.equal(this.pending.length, 0, "more than one write is in flight");
      this.pending.push({ text, complete }); this.events.push(`write:${text}`);
    }
    finish() { const next = this.pending.shift()!; this.screen += next.text; next.complete(); }
  }
  const api = load("pages/CompileTerminal.tsx", {
    react: h.react, "@xterm/xterm": { Terminal: FakeTerminal },
    "@xterm/addon-fit": { FitAddon: class { fit() {} } },
  });
  const handle = { current: null };
  const render = (text: string, ref = handle) => {
    const tree = h.render(() => api.CompileTerminal({ text, query: "", follow: true, onFollow() {}, empty: "", handle: ref }));
    elements(tree).find((element) => element.props.className === "compile-terminal-screen")!.props.ref.current = {};
    h.flush();
  };
  render("old log");
  const view = views[0];
  render("replacement");
  render("latest log");
  assert.deepEqual(view.events, ["write:old log"]);
  view.finish();
  assert.deepEqual(view.events, ["write:old log", "reset", "write:latest log"]);
  render("latest log extended");
  view.finish();
  assert.equal(view.screen, "latest log");
  assert.equal(view.pending[0].text, " extended");
  view.finish();
  assert.equal(view.screen, "latest log extended");
  render("");
  assert.equal(view.screen, "");
  view.finish();
  assert.equal(view.screen, "");
  render("pending before remount");
  render("new terminal", { current: null });
  const replacement = views[1];
  view.finish();
  assert.equal(replacement.pending.length, 1);
  replacement.finish();
  assert.equal(replacement.screen, "new terminal");
  render("new terminal tail", { current: null });
  const disposed = views[2];
  h.unmount();
  disposed.finish();
  assert.equal(disposed.pending.length, 0);
});
