import { firmwareForModel, type ModelFirmware, type ModelsProvider } from "@gizclaw/models-player";

/** Keep provider admission authoritative while making enter() install exactly the user's choice. */
export function previewFirmwareProvider(
  provider: ModelsProvider,
  selected: string | null,
  onChoices: (choices: readonly ModelFirmware[]) => void,
): ModelsProvider {
  return {
    async listFirmwares(model, context) {
      const choices = await provider.listFirmwares(model, context);
      if (choices.some((firmware) => firmware.model !== model)) throw new Error("固件目录返回了其他型号的固件。");
      onChoices(choices);
      // ModelPlay.enter() installs its first admitted entry. Limit that session to the
      // selected entry instead of downloading a default firmware before the requested one.
      return selected ? [firmwareForModel(model, selected, choices)] : choices;
    },
    resolveFirmware(firmware, context) {
      return provider.resolveFirmware(firmware, context);
    },
  };
}
