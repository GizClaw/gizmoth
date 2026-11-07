export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

/** Move among available options, skipping disabled items and wrapping at the ends. */
export function nextSelectValue(options: readonly SelectOption[], current: string, step: -1 | 1) {
  const enabled = options.filter((option) => !option.disabled);
  if (!enabled.length) return undefined;
  const index = enabled.findIndex((option) => option.value === current);
  if (index < 0) return (step > 0 ? enabled[0] : enabled.at(-1))!.value;
  return enabled[(index + step + enabled.length) % enabled.length].value;
}

/** Prefix matching for the select-only combobox's keyboard typeahead. */
export function matchSelectValue(options: readonly SelectOption[], query: string, current: string) {
  const enabled = options.filter((option) => !option.disabled);
  const prefix = query.toLocaleLowerCase();
  if (!prefix || !enabled.length) return undefined;
  const index = enabled.findIndex((option) => option.value === current);
  // Extending a prefix refines the current match; only single-character searches cycle.
  if (prefix.length > 1 && index >= 0 && enabled[index].label.toLocaleLowerCase().startsWith(prefix)) return current;
  for (let offset = 1; offset <= enabled.length; offset++) {
    const option = enabled[(Math.max(-1, index) + offset) % enabled.length];
    if (option.label.toLocaleLowerCase().startsWith(prefix)) return option.value;
  }
  return undefined;
}

/** Viewport-safe popup placement, including triggers near the bottom of a dialog. */
export function selectPopupPosition(
  anchor: { left: number; top: number; bottom: number; width: number },
  viewport: { width: number; height: number },
  optionCount: number,
  minimumWidth = 160,
) {
  const margin = 8;
  const gap = 4;
  const width = Math.min(Math.max(anchor.width, minimumWidth), Math.max(0, viewport.width - margin * 2));
  const desired = Math.min(300, optionCount * 32 + 12);
  const below = Math.max(0, viewport.height - anchor.bottom - gap - margin);
  const above = Math.max(0, anchor.top - gap - margin);
  const upwards = below < Math.min(desired, 160) && above > below;
  return {
    left: Math.max(margin, Math.min(anchor.left, viewport.width - width - margin)),
    width,
    maxHeight: Math.min(300, upwards ? above : below),
    ...(upwards ? { bottom: viewport.height - anchor.top + gap } : { top: anchor.bottom + gap }),
  };
}
