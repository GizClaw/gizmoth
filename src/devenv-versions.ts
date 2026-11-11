import type { DevEnvInstallation, DevEnvToolchain, DevEnvVersion } from "./devenv.ts";

/** Installing the default must not hide other releases of the same tool. */
export function uninstalledVersions(toolchain: DevEnvToolchain, installations: DevEnvInstallation[]): DevEnvVersion[] {
  return toolchain.versions.filter(
    (release) => !installations.some((record) => record.id === toolchain.id && record.version === release.version),
  );
}

export function selectedVersion(
  versions: DevEnvVersion[],
  preferred: string | undefined,
  defaultVersion: string | null,
): DevEnvVersion | null {
  return (
    versions.find((release) => release.version === preferred) ??
    versions.find((release) => release.version === defaultVersion) ??
    versions[0] ??
    null
  );
}
