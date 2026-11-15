import { t, useI18n } from "../i18n";
import { useEffect, useId, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { DotsThree } from "@phosphor-icons/react";
import { IconButton } from "../ui";
import "./WorkspaceNavigation.css";

type Action = {
  label: string;
  icon: ReactNode;
  onSelect: () => void;
  disabled?: boolean;
  danger?: boolean;
  separator?: boolean;
  title?: string;
};

/** Small contextual actions shared by the project titlebar and code-pane header. */
export function WorkspaceActions({
  label,
  actions,
  disabled = false,
}: {
  label: string;
  actions: Action[];
  disabled?: boolean;
}) {
  useI18n();
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<CSSProperties>({});
  const close = (restore = true) => {
    menu.current?.hidePopover();
    if (restore) trigger.current?.focus();
  };
  useEffect(() => {
    if (!open) return;
    if (disabled) {
      close(false);
      return;
    }
    menu.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
    const resize = () => close(false);
    const scroll = (event: Event) => {
      if (event.target instanceof Node && menu.current?.contains(event.target)) return;
      close(false);
    };
    window.addEventListener("resize", resize);
    window.addEventListener("scroll", scroll, true);
    return () => {
      window.removeEventListener("resize", resize);
      window.removeEventListener("scroll", scroll, true);
    };
  }, [open, disabled]);
  return (
    <>
      <IconButton
        ref={trigger}
        title={t(label)}
        aria-label={t(label)}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={id}
        disabled={disabled}
        onClick={() => {
          if (open) {
            close();
            return;
          }
          const rect = trigger.current!.getBoundingClientRect();
          const below = innerHeight - rect.bottom - 14;
          const above = rect.top - 14;
          const openAbove = below < 112 && above > below;
          setPosition({
            top: openAbove ? undefined : rect.bottom + 6,
            bottom: openAbove ? innerHeight - rect.top + 6 : undefined,
            left: Math.max(8, Math.min(rect.right - 208, innerWidth - 216)),
            maxHeight: Math.max(32, openAbove ? above : below),
          });
          menu.current?.showPopover();
        }}
      >
        <DotsThree size={17} />
      </IconButton>
      <div
        ref={menu}
        id={id}
        popover="auto"
        role="menu"
        aria-label={t(label)}
        className="workspace-action-menu"
        style={position}
        onToggle={(event) => setOpen(event.newState === "open")}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing || event.keyCode === 229) return;
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            close();
            return;
          }
          if (event.key === "Tab") {
            close(false);
            return;
          }
          const items = [...event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
          if (!items.length) return;
          const current = items.indexOf(document.activeElement as HTMLButtonElement);
          let next: number;
          if (event.key === "ArrowDown") next = (current + 1) % items.length;
          else if (event.key === "ArrowUp") next = (current - 1 + items.length) % items.length;
          else if (event.key === "Home") next = 0;
          else if (event.key === "End") next = items.length - 1;
          else return;
          event.preventDefault();
          items[next].focus();
        }}
      >
        {actions.map((action) => (
          <button
            key={action.label}
            type="button"
            role="menuitem"
            className={`workspace-action${action.danger ? " is-danger" : ""}${action.separator ? " has-separator" : ""}`}
            disabled={action.disabled}
            title={action.title}
            onClick={() => {
              close();
              action.onSelect();
            }}
          >
            {action.icon}
            <span>{t(action.label)}</span>
          </button>
        ))}
      </div>
    </>
  );
}
