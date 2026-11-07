export type PreviewButton = "left" | "right" | "ok" | "back" | "power" | "record" | "volume_down" | "volume_up";

// Arrows and Escape are game controls only inside the focused preview.
// SDK physical-key aliases retain their device meanings; Up/Down are not invented.
const keys: Record<string, PreviewButton> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  Enter: "ok",
  NumpadEnter: "ok",
  Escape: "back",
  Backspace: "back",
  KeyD: "left",
  KeyJ: "left",
  KeyF: "right",
  KeyK: "right",
  KeyE: "back",
  KeyU: "back",
  KeyR: "ok",
  KeyI: "ok",
  KeyC: "volume_down",
  KeyM: "volume_down",
  KeyV: "volume_up",
  Comma: "volume_up",
  Space: "record",
  KeyB: "power",
};

export function previewKey(event: {
  code: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
  isComposing?: boolean;
}): PreviewButton | undefined {
  return event.ctrlKey || event.metaKey || event.altKey || event.isComposing ? undefined : keys[event.code];
}

export interface PreviewButtonSink {
  press(button: PreviewButton, source: string): void;
  release(button: PreviewButton, source: string): void;
}

export function createPreviewInput(sink: PreviewButtonSink) {
  const held = new Map<string, PreviewButton>();
  const release = (source: string) => {
    const button = held.get(source);
    if (!button) return;
    held.delete(source);
    sink.release(button, source);
  };
  return {
    press(button: PreviewButton, source: string) {
      if (held.get(source) === button) return;
      release(source);
      held.set(source, button);
      sink.press(button, source);
    },
    release,
    releaseAll() {
      for (const source of held.keys()) release(source);
    },
  };
}
