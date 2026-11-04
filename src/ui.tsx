import { useEffect, useRef, type ReactNode } from "react";
import { ChevronRight, X } from "lucide-react";

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
  glass,
}: {
  crumbs: { label: string; onClick?: () => void }[];
  actions?: ReactNode;
  glass?: boolean;
}) {
  return (
    <header className={glass ? "page-header glass" : "page-header"} data-tauri-drag-region>
      <nav className="crumbs" aria-label="位置">
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
}

export function Dialog({
  title,
  onClose,
  children,
  wide,
  busy,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  wide?: boolean;
  busy?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    ref.current?.showModal();
  }, []);
  return (
    <dialog
      ref={ref}
      className={wide ? "dialog wide" : "dialog"}
      aria-label={title}
      onCancel={(e) => {
        e.preventDefault();
        if (!busy) onClose();
      }}
    >
      <div className="dialog-head">
        <h2>{title}</h2>
        <button className="icon-button" aria-label="关闭" disabled={busy} onClick={onClose}>
          <X size={16} />
        </button>
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
  if (seconds < 60) return "刚刚";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} 小时前`;
  return `${Math.floor(seconds / 86400)} 天前`;
}
