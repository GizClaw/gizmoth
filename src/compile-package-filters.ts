import type { CompilePackage } from "./compile.ts";

const preferred = ["arm64", "x86_64", "esp32", "jieli", "beken"];
const names: Record<string, string> = {
  arm64: "ARM64",
  x86_64: "x86_64",
  esp32: "ESP32",
  jieli: "Jieli",
  beken: "Beken",
};

export const packageLabelName = (label: string) => names[label] ?? label;

export function packageLabels(item: CompilePackage, platform: string): string[] {
  // Older services may omit labels. Their declared platform is the only known fallback.
  return [...new Set(item.labels ?? [(item.platform ?? platform).replace(/^linux-/, "")])];
}

/** Catalog previews do not represent a second download of an already managed package. */
export function managedPackages(items: CompilePackage[], platform: string): CompilePackage[] {
  return items.filter((item) => (item.platform ?? platform) === platform || Boolean(item.path));
}

export function packageFilters(items: CompilePackage[], platform: string) {
  const counts = new Map<string, number>();
  for (const item of items) {
    for (const label of packageLabels(item, platform)) counts.set(label, (counts.get(label) ?? 0) + 1);
  }
  const rank = (label: string) => (preferred.includes(label) ? preferred.indexOf(label) : preferred.length);
  return [...counts].sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b));
}

export function filterPackages(items: CompilePackage[], platform: string, label: string): CompilePackage[] {
  return label === "all" ? items : items.filter((item) => packageLabels(item, platform).includes(label));
}
