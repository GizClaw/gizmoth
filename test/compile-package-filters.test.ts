import assert from "node:assert/strict";
import test from "node:test";
import type { CompilePackage } from "../src/compile.ts";
import {
  filterPackages,
  managedPackages,
  packageFilters,
  packageLabels,
  packageLabelName,
} from "../src/compile-package-filters.ts";

const platform = "linux-arm64";
const item = (id: string, overrides: Partial<CompilePackage> = {}): CompilePackage => ({
  kind: "toolchain",
  id,
  name: id,
  version: "1",
  default: true,
  download: 10,
  platform,
  path: `/opt/gizmoth/packages/toolchains/${id}/1`,
  state: "installed",
  ...overrides,
});

test("execution labels select translated Jieli independently of recipe platform", () => {
  const jieli = item("jieli", { labels: ["x86_64", "jieli"] });
  const qemu = item("linux-qemu-user", { labels: ["arm64"] });
  const esp = item("riscv32-esp-elf", { labels: ["arm64", "esp32"] });
  const items = [jieli, qemu, esp];
  assert.deepEqual(filterPackages(items, platform, "x86_64"), [jieli]);
  assert.deepEqual(filterPackages(items, platform, "arm64"), [qemu, esp]);
  assert.deepEqual(filterPackages(items, platform, "jieli"), [jieli]);
  assert.deepEqual(filterPackages(items, platform, "esp32"), [esp]);
  assert.equal(filterPackages(items, platform, "all"), items);
  assert.deepEqual(filterPackages(items, platform, "unknown"), []);
});

test("managed foreign packages remain visible while unsupported catalog previews do not", () => {
  const native = item("native");
  const managed = item("foreign", { platform: "linux-x86_64", labels: ["x86_64"] });
  const preview = item("native", { platform: "linux-x86_64", path: "", state: "unsupported" });
  const removed = item("removed", { state: "removed", labels: ["x86_64", "jieli"] });
  assert.deepEqual(managedPackages([native, managed, preview, removed], platform), [native, managed, removed]);
});

test("filter counts deduplicate labels and order architecture then ecosystem", () => {
  const items = [
    item("jieli", { labels: ["jieli", "x86_64", "x86_64"] }),
    item("esp", { labels: ["esp32", "arm64"] }),
    item("beken", { labels: ["beken", "arm64"] }),
    item("data", { labels: [] }),
  ];
  assert.deepEqual(packageFilters(items, platform), [
    ["arm64", 2],
    ["x86_64", 1],
    ["esp32", 1],
    ["jieli", 1],
    ["beken", 1],
  ]);
  assert.deepEqual(packageLabels(items[3], platform), []);
  assert.deepEqual(packageFilters([], platform), []);
  assert.deepEqual(packageLabels(item("legacy"), platform), ["arm64"]);
  assert.equal(packageLabelName("esp32"), "ESP32");
  assert.equal(packageLabelName("other"), "other");
});
