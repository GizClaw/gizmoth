import { t, useI18n } from "../i18n";
import { useRef, type KeyboardEvent, type PointerEvent, type RefObject } from "react";

export const CHAT_MIN = 300;
export const CODE_MIN = 380;
export const CHAT_DEFAULT = 420;
const STEP = 16;

/** Keeps the chat column usable without starving the editor. */
export function clampChatWidth(value: number, total: number) {
  const max = Math.max(CHAT_MIN, total - CODE_MIN);
  return Math.round(Math.min(Math.max(value, CHAT_MIN), max));
}

/**
 * Vertical divider between the chat and code panes. Drag with a pointer, use
 * arrow keys (Shift for larger steps), Home/End for the limits, and
 * double-click or Enter to restore the default width.
 */
export function PaneSplitter({
  container,
  width,
  controls,
  onResize,
  onCommit,
}: {
  container: RefObject<HTMLElement | null>;
  width: number;
  controls: string;
  onResize: (width: number) => void;
  onCommit: (width: number) => void;
}) {
  useI18n();
  const drag = useRef<{ pointer: number; x: number; start: number; last: number } | null>(null);
  const total = () => container.current?.clientWidth ?? 0;
  const max = Math.max(CHAT_MIN, total() - CODE_MIN);

  function finish(event: PointerEvent<HTMLDivElement>) {
    const current = drag.current;
    if (!current || current.pointer !== event.pointerId) return;
    drag.current = null;
    document.documentElement.classList.remove("is-resizing-panes");
    onCommit(current.last);
  }
  function key(event: KeyboardEvent<HTMLDivElement>) {
    const step = event.shiftKey ? STEP * 4 : STEP;
    const next =
      event.key === "ArrowLeft"
        ? width - step
        : event.key === "ArrowRight"
          ? width + step
          : event.key === "Home"
            ? CHAT_MIN
            : event.key === "End"
              ? Infinity
              : event.key === "Enter"
                ? CHAT_DEFAULT
                : null;
    if (next === null) return;
    event.preventDefault();
    onCommit(clampChatWidth(next, total()));
  }

  return (
    <div
      className="workspace-splitter"
      role="separator"
      aria-orientation="vertical"
      aria-label={t("调整对话与代码的宽度")}
      aria-controls={controls}
      aria-valuemin={CHAT_MIN}
      aria-valuemax={max}
      aria-valuenow={Math.min(width, max)}
      title={t("拖动调整宽度，双击恢复默认")}
      tabIndex={0}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        // Start from the rendered width so a stored value clamped by CSS never jumps.
        const rendered = (event.currentTarget.previousElementSibling as HTMLElement | null)?.getBoundingClientRect()
          .width;
        const start = clampChatWidth(rendered ?? width, total());
        drag.current = { pointer: event.pointerId, x: event.clientX, start, last: start };
        event.currentTarget.setPointerCapture(event.pointerId);
        document.documentElement.classList.add("is-resizing-panes");
      }}
      onPointerMove={(event) => {
        const current = drag.current;
        if (!current || current.pointer !== event.pointerId) return;
        current.last = clampChatWidth(current.start + event.clientX - current.x, total());
        onResize(current.last);
      }}
      onPointerUp={finish}
      onPointerCancel={finish}
      onLostPointerCapture={finish}
      onDoubleClick={() => onCommit(clampChatWidth(CHAT_DEFAULT, total()))}
      onKeyDown={key}
    />
  );
}
