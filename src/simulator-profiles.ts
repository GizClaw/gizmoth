import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DeviceModel } from "./device-model";

export const simulatorModels: DeviceModel[] = [
  { id: "h106-tiga", name: "H106 Tiga" },
  { id: "h106-zero", name: "H106 Zero" },
  { id: "h106-orb", name: "H106 Orb" },
  { id: "h106-zeta", name: "H106 Zeta" },
];
export interface SimulatorProfile {
  id: string;
  name: string;
  model: DeviceModel["id"];
  firmwareId: string;
  view: "model" | "screen";
}
export type SimulatorPatch = Partial<Pick<SimulatorProfile, "name" | "firmwareId" | "view">>;

/** Explicit IDs are strict; a deleted remembered target falls back to another simulator or creation. */
export function selectSimulatorProfile(
  profiles: readonly SimulatorProfile[],
  explicitId?: string,
  rememberedId?: string,
): SimulatorProfile | undefined {
  if (explicitId !== undefined) {
    const profile = profiles.find((item) => item.id === explicitId);
    if (!profile) throw new Error("Simulator does not exist. Call list_simulators before selecting another.");
    return profile;
  }
  return profiles.find((item) => item.id === rememberedId) ?? profiles[0];
}
const key = "gizmoth.simulators.v1";
// Pick only device preferences so legacy project links are removed on the next write.
export function profilePreferences(profile: SimulatorProfile): SimulatorProfile {
  const { id, name, model, firmwareId, view } = profile;
  return { id, name, model, firmwareId, view };
}
const read = (): SimulatorProfile[] => JSON.parse(localStorage.getItem(key) ?? "[]").map(profilePreferences);
function validate(profile: SimulatorProfile) {
  if (
    !profile.name.trim() ||
    profile.name.length > 80 ||
    !simulatorModels.some((m) => m.id === profile.model) ||
    !["model", "screen"].includes(profile.view) ||
    profile.firmwareId.length > 256
  )
    throw new Error("模拟器配置无效：名称限 1–80 字，型号必须受支持。");
}
function changed() {
  window.dispatchEvent(new Event("simulator-profiles-changed"));
}
// Browser writes are synchronous, so closing immediately after a selection cannot drop it.
export const simulatorProfiles = {
  list: async (): Promise<SimulatorProfile[]> => (isTauri() ? invoke("simulator_profiles_list") : read()),
  create: async (name: string, model: DeviceModel["id"]): Promise<SimulatorProfile> => {
    const profile: SimulatorProfile = {
      id: crypto.randomUUID(),
      name: name.trim(),
      model,
      firmwareId: "",
      view: "model",
    };
    validate(profile);
    if (isTauri()) {
      const result = await invoke<SimulatorProfile>("simulator_profile_create", { profile });
      changed();
      return result;
    }
    localStorage.setItem(key, JSON.stringify([...read(), profile]));
    changed();
    return profile;
  },
  update: async (id: string, patch: SimulatorPatch): Promise<SimulatorProfile> => {
    if (Object.keys(patch).some((field) => !["name", "firmwareId", "view"].includes(field)))
      throw new Error("模拟器机型创建后不可更改，请新建模拟器。");
    if (isTauri()) {
      const result = await invoke<SimulatorProfile>("simulator_profile_update", { id, patch });
      changed();
      return result;
    }
    const all = read();
    const index = all.findIndex((p) => p.id === id);
    if (index < 0) throw new Error("此模拟器已删除。");
    const profile = profilePreferences({ ...all[index], ...patch, id });
    validate(profile);
    all[index] = profile;
    localStorage.setItem(key, JSON.stringify(all));
    changed();
    return profile;
  },
  delete: async (id: string): Promise<void> => {
    if (isTauri()) await invoke("simulator_profile_delete", { id });
    else localStorage.setItem(key, JSON.stringify(read().filter((p) => p.id !== id)));
    changed();
  },
};
export function watchSimulatorProfiles(refresh: () => void) {
  const nativeWatch = isTauri() ? listen("simulator-profiles-changed", refresh) : null;
  window.addEventListener("storage", refresh);
  window.addEventListener("simulator-profiles-changed", refresh);
  window.addEventListener("focus", refresh);
  return () => {
    void nativeWatch?.then((unlisten) => unlisten()).catch(() => {});
    window.removeEventListener("storage", refresh);
    window.removeEventListener("simulator-profiles-changed", refresh);
    window.removeEventListener("focus", refresh);
  };
}
export function showSimulatorManager() {
  window.dispatchEvent(new Event("open-simulator-manager"));
}
