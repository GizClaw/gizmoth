import {
  useEffect,
  useId,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type ComponentPropsWithRef,
  type CSSProperties,
  type KeyboardEvent,
} from "react";
import { createPortal } from "react-dom";
import { CaretDown, Check } from "@phosphor-icons/react";
import { matchSelectValue, nextSelectValue, selectPopupPosition, type SelectOption } from "./select-options.ts";
import type { ControlSize } from "./ui-controls";
import "./ui-controls.css";
import "./Select.css";
import { useI18n } from "./i18n";

export type { SelectOption } from "./select-options.ts";
export type SelectProps = Omit<ComponentPropsWithRef<"button">, "value" | "onChange" | "children" | "type"> & {
  value: string;
  onValueChange: (value: string) => void;
  options: readonly SelectOption[];
  placeholder?: string;
  controlSize?: ControlSize;
  variant?: "default" | "quiet";
};

/**
 * Shared, controlled, select-only combobox. Its options are HTML in the WebView,
 * never an OS menu. A native Popover supplies layering/dismissal, not menu artwork.
 */
export function Select({
  value,
  onValueChange,
  options,
  placeholder,
  controlSize = "md",
  variant = "default",
  className,
  style,
  disabled = false,
  ref,
  name,
  form,
  onClick,
  onKeyDown,
  onBlur,
  ...props
}: SelectProps) {
  const { t } = useI18n();
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const popup = useRef<HTMLDivElement>(null);
  const placedAnchor = useRef<DOMRect | null>(null);
  const [container, setContainer] = useState<HTMLElement | null>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(value);
  const [position, setPosition] = useState<CSSProperties>({});
  const query = useRef({ text: "", at: 0 });
  const nativePopover = typeof HTMLElement !== "undefined" && typeof HTMLElement.prototype.showPopover === "function";
  const unavailable = disabled || !options.some((option) => !option.disabled);
  const selected = options.find((option) => option.value === value);
  const activeIndex = options.findIndex((option) => option.value === active);
  const popupId = `${id}-options`;
  useImperativeHandle(ref, () => trigger.current!, []);

  useLayoutEffect(() => {
    // Keep menus inside their owning overlay so its outside-click handler does
    // not dismiss the parent; stay outside wrapping labels to avoid reactivation.
    setContainer(trigger.current?.closest<HTMLElement>("dialog, [popover], [data-popover-scope]") ?? document.body);
  }, []);

  function close(restoreFocus = false) {
    if (nativePopover && popup.current?.isConnected && popup.current.matches(":popover-open"))
      popup.current.hidePopover();
    setOpen(false);
    query.current = { text: "", at: 0 };
    if (restoreFocus) trigger.current?.focus();
  }
  function place() {
    if (!trigger.current) return;
    const bounds = trigger.current.getBoundingClientRect();
    placedAnchor.current = bounds;
    setPosition(selectPopupPosition(bounds, { width: innerWidth, height: innerHeight }, options.length));
  }
  function show(next = value) {
    if (unavailable || !popup.current) return;
    // Safari on macOS does not focus buttons on pointer click. Keep keyboard navigation explicit.
    trigger.current?.focus({ preventScroll: true });
    const enabled = options.find((option) => option.value === next && !option.disabled);
    setActive(enabled?.value ?? options.find((option) => !option.disabled)?.value ?? "");
    place();
    setOpen(true);
    query.current = { text: "", at: 0 };
    if (nativePopover) popup.current.showPopover();
  }
  function choose(next: string, restoreFocus = true) {
    if (unavailable || !options.some((option) => option.value === next && !option.disabled)) return;
    close(restoreFocus);
    if (next !== value) onValueChange(next);
  }

  useEffect(() => {
    if (open && unavailable) close();
  }, [open, unavailable]);
  useEffect(() => {
    if (!open) return;
    if (!options.some((option) => option.value === active && !option.disabled))
      setActive(options.find((option) => !option.disabled)?.value ?? "");
  }, [open, options, active]);
  useLayoutEffect(() => {
    if (open && activeIndex >= 0)
      popup.current
        ?.querySelector<HTMLElement>(`[data-option-index="${activeIndex}"]`)
        ?.scrollIntoView({ block: "nearest" });
  }, [open, activeIndex]);
  useEffect(() => {
    if (!open) return;
    const dismiss = () => close();
    const scroll = (event: Event) => {
      if (popup.current?.contains(event.target as Node)) return;
      const previous = placedAnchor.current;
      const current = trigger.current?.getBoundingClientRect();
      // Ignore unrelated panes and delayed scroll events from before opening.
      // Dismiss only when the anchor actually moved away from the placed menu.
      if (
        !previous ||
        !current ||
        (["left", "top", "bottom", "width"] as const).some((key) => Math.abs(current[key] - previous[key]) > 0.5)
      )
        close();
    };
    const outside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!popup.current?.contains(target) && !trigger.current?.contains(target)) close();
    };
    const observer = new ResizeObserver(() => place());
    if (trigger.current) observer.observe(trigger.current);
    window.addEventListener("resize", dismiss);
    window.addEventListener("scroll", scroll, true);
    document.addEventListener("pointerdown", outside);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", dismiss);
      window.removeEventListener("scroll", scroll, true);
      document.removeEventListener("pointerdown", outside);
    };
  }, [open, options.length]);

  function key(event: KeyboardEvent<HTMLButtonElement>) {
    onKeyDown?.(event);
    if (event.defaultPrevented || event.nativeEvent.isComposing || event.keyCode === 229) return;
    if (event.key === "Escape" && open) {
      event.preventDefault();
      event.stopPropagation();
      close(true);
      return;
    }
    if (event.key === "Tab" && open) {
      choose(active, false);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!open) show();
      else setActive(nextSelectValue(options, active, event.key === "ArrowDown" ? 1 : -1) ?? active);
      return;
    }
    if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      const enabled = options.filter((option) => !option.disabled);
      const next = (event.key === "Home" ? enabled[0] : enabled.at(-1))?.value;
      if (next !== undefined) {
        if (open) setActive(next);
        else show(next);
      }
      return;
    }
    if ((event.key === "Enter" || event.key === " ") && !event.altKey && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      if (open) choose(active);
      else show();
      return;
    }
    if (event.key.length === 1 && !event.altKey && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      const now = Date.now();
      const previous = now - query.current.at < 700 ? query.current.text : "";
      const character = event.key.toLocaleLowerCase();
      const text = previous && [...previous].every((value) => value === character) ? character : previous + character;
      query.current = { text, at: now };
      const next = matchSelectValue(options, text, open ? active : value);
      if (next !== undefined) {
        if (open) setActive(next);
        else if (next !== value) onValueChange(next);
      }
    }
  }

  return (
    <span
      className={["ui-select", `control-${controlSize}`, variant === "quiet" && "ui-select-quiet", className]
        .filter(Boolean)
        .join(" ")}
      style={style}
      data-disabled={unavailable || undefined}
    >
      <button
        {...props}
        ref={trigger}
        type="button"
        role="combobox"
        className="ui-select-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={popupId}
        aria-activedescendant={open && activeIndex >= 0 ? `${id}-option-${activeIndex}` : undefined}
        data-value={value}
        disabled={unavailable}
        form={form}
        onClick={(event) => {
          onClick?.(event);
          if (event.defaultPrevented) return;
          event.stopPropagation();
          if (open) close(true);
          else show();
        }}
        onKeyDown={key}
        onBlur={(event) => {
          onBlur?.(event);
          if (!popup.current?.contains(event.relatedTarget as Node | null)) close();
        }}
      >
        <span className="ui-select-value">{selected?.label ?? placeholder ?? t("请选择")}</span>
        <CaretDown className="ui-select-caret" size={14} weight="regular" aria-hidden="true" />
      </button>
      {name && <input type="hidden" name={name} value={value} form={form} disabled={unavailable} />}
      {container &&
        createPortal(
          <div
            ref={popup}
            id={popupId}
            popover={nativePopover ? "auto" : undefined}
            role="listbox"
            aria-label={props["aria-label"] ?? props.title ?? "选项"}
            className="ui-select-menu"
            style={position}
            data-select-menu=""
            data-fallback-open={(!nativePopover && open) || undefined}
            onToggle={(event) => {
              setOpen(event.newState === "open");
            }}
          >
            {options.map((option, index) => (
              <div
                key={option.value}
                id={`${id}-option-${index}`}
                role="option"
                aria-label={option.label}
                aria-selected={option.value === value}
                aria-disabled={option.disabled || undefined}
                data-value={option.value}
                data-option-index={index}
                className={`ui-select-option${active === option.value ? " is-active" : ""}`}
                title={option.label}
                onPointerMove={(event) => {
                  if (event.pointerType !== "touch" && !option.disabled) setActive(option.value);
                }}
                onPointerDown={(event) => {
                  if (event.pointerType !== "touch") event.preventDefault();
                }}
                onClick={(event) => {
                  event.stopPropagation();
                  choose(option.value);
                }}
              >
                <span className="ui-select-option-label">{option.label}</span>
                {option.value === value && <Check size={14} aria-hidden="true" />}
              </div>
            ))}
          </div>,
          container,
        )}
    </span>
  );
}
