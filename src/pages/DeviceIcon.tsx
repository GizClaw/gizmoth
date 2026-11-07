import { Cpu } from "@phosphor-icons/react";
import { getModelIcon, type PlayerModelId } from "@gizclaw/models-player";

/** Bundled SDK artwork stays sharp and inherits the current light/dark palette. */
export function DeviceIcon({ model }: { model?: PlayerModelId }) {
  const source = model ? getModelIcon(model) : undefined;
  return source ? (
    <span
      className="device-model-glyph"
      aria-hidden="true"
      data-model-icon={model}
      style={{ maskImage: `url("${source}")`, WebkitMaskImage: `url("${source}")` }}
    />
  ) : (
    <Cpu size={20} aria-hidden="true" />
  );
}
