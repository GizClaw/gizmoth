import { IconButton } from "./ui-controls";
import { t, useI18n } from "./i18n";
import {
  createContext,
  useContext,
  useEffect,
  useRef,
  type CSSProperties,
  type MouseEvent,
  type PointerEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { CaretRight as ChevronRight, X } from "@phosphor-icons/react";

export { Button, IconButton, Input, SearchInput, Select } from "./ui-controls";
export type {
  ButtonProps,
  IconButtonProps,
  InputProps,
  SelectProps,
  SelectOption,
  SearchInputProps,
  ControlSize,
} from "./ui-controls";

export const TitlebarContext = createContext<HTMLElement | null>(null);

/** Single-colour Gizmoth mark (branding/glyph.svg). */
export function Mark({ size = 20 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="32 32 192 192" aria-hidden="true">
      <g transform="translate(1 16) rotate(-10 128 128)" fill="currentColor">
        <path d="M103 164 L83 119 L43 99 L51 140 L79 166 Z" />
        <path d="M148 157 L137 96 L112 28 L74 69 L88 111 L108 160 Z" />
        <path d="M169 162 L160 157 L117 167 L82 182 L120 185 L166 180 L172 173 Z" />
        <path d="M201 158 L194 146 L180 146 L173 158 L180 170 L194 170 Z" />
        <path
          d="M190 147 L194 123 L207 110"
          fill="none"
          stroke="currentColor"
          strokeWidth="5"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
        <path d="M207 103 L214 110 L207 117 L200 110 Z" />
      </g>
    </svg>
  );
}

export function PageHeader({
  crumbs,
  actions,
}: {
  crumbs: { label: string; onClick?: () => void }[];
  actions?: ReactNode;
}) {
  const { t } = useI18n();
  const titlebar = useContext(TitlebarContext);
  const header = (
    <header className="page-header" data-tauri-drag-region>
      <nav className="crumbs" aria-label={t("位置")}>
        {crumbs.map((crumb, index) => (
          <span key={index} className={index === crumbs.length - 1 ? "crumb current" : "crumb"}>
            {index > 0 && <span className="crumb-sep">/</span>}
            {crumb.onClick ? (
              <button className="crumb-link" onClick={crumb.onClick}>
                {crumb.label}
              </button>
            ) : (
              crumb.label
            )}
          </span>
        ))}
      </nav>
      <div className="header-actions">{actions}</div>
    </header>
  );
  return titlebar ? createPortal(header, titlebar) : header;
}

export function Dialog({
  title,
  onClose,
  children,
  wide,
  busy,
  className,
  style,
  heading,
  initialFocus,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  wide?: boolean;
  busy?: boolean;
  className?: string;
  style?: CSSProperties;
  heading?: ReactNode;
  initialFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useI18n();
  const ref = useRef<HTMLDialogElement>(null);
  const returnFocus = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const backdropPointer = useRef<number | null>(null);
  const backdropClick = useRef(false);
  const onBackdrop = (event: PointerEvent<HTMLDialogElement> | MouseEvent<HTMLDialogElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect();
    return (
      event.target === event.currentTarget &&
      (event.clientX < bounds.left ||
        event.clientX > bounds.right ||
        event.clientY < bounds.top ||
        event.clientY > bounds.bottom)
    );
  };
  useEffect(() => {
    const dialog = ref.current;
    dialog?.showModal();
    initialFocus?.current?.focus({ preventScroll: true });
    return () => {
      dialog?.close();
      if (returnFocus.current?.isConnected) returnFocus.current.focus({ preventScroll: true });
    };
  }, [initialFocus]);
  return (
    <dialog
      ref={ref}
      className={`dialog${wide ? " wide" : ""}${className ? ` ${className}` : ""}`}
      style={style}
      aria-label={title}
      onCancel={(e) => {
        e.preventDefault();
        if (!busy) onClose();
      }}
      onPointerDown={(event) => {
        backdropClick.current = false;
        backdropPointer.current = !busy && onBackdrop(event) ? event.pointerId : null;
      }}
      onPointerUp={(event) => {
        backdropClick.current = backdropPointer.current === event.pointerId && !busy && onBackdrop(event);
        backdropPointer.current = null;
      }}
      onClick={(event) => {
        const shouldClose = backdropClick.current && !busy && onBackdrop(event);
        backdropClick.current = false;
        if (shouldClose) {
          event.preventDefault();
          event.stopPropagation();
          onClose();
        }
      }}
      onPointerCancel={() => {
        backdropPointer.current = null;
        backdropClick.current = false;
      }}
    >
      <div className="dialog-head">
        {heading ?? <h2>{title}</h2>}
        <IconButton aria-label={t("关闭")} disabled={busy} onClick={onClose}>
          <X size={16} />
        </IconButton>
      </div>
      {children}
    </dialog>
  );
}

export function Empty({
  icon,
  title,
  body,
  action,
}: {
  icon: ReactNode;
  title: string;
  body: string;
  action?: ReactNode;
}) {
  return (
    <div className="empty">
      <div className="empty-icon">{icon}</div>
      <h2>{title}</h2>
      <p>{body}</p>
      {action}
    </div>
  );
}

export function Row({
  icon,
  title,
  meta,
  trailing,
  onClick,
}: {
  icon?: ReactNode;
  title: ReactNode;
  meta?: ReactNode;
  trailing?: ReactNode;
  onClick?: () => void;
}) {
  const body = (
    <>
      {icon && <span className="row-icon">{icon}</span>}
      <span className="row-main">
        <span className="row-title">{title}</span>
        {meta && <span className="row-meta">{meta}</span>}
      </span>
      {trailing && <span className="row-trailing">{trailing}</span>}
      {onClick && <ChevronRight className="row-chevron" size={15} />}
    </>
  );
  return onClick ? (
    <button className="row clickable" onClick={onClick}>
      {body}
    </button>
  ) : (
    <div className="row">{body}</div>
  );
}

export function Dot({ tone }: { tone: "ok" | "off" | "warn" | "idle" }) {
  return <i className={`dot dot-${tone}`} aria-hidden="true" />;
}

export function relativeTime(ms: number) {
  if (!ms) return "";
  const seconds = Math.round((Date.now() - ms) / 1000);
  if (seconds < 60) return t("刚刚");
  if (seconds < 3600) return t("{count} 分钟前", { count: Math.floor(seconds / 60) });
  if (seconds < 86400) return t("{count} 小时前", { count: Math.floor(seconds / 3600) });
  return t("{count} 天前", { count: Math.floor(seconds / 86400) });
}
