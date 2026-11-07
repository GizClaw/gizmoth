import { useId, useState, type ReactNode } from "react";
import { Info } from "@phosphor-icons/react";
import { useI18n } from "../i18n";

/** Keep the setting and its control on one line, with help available on focus or hover. */
export function SettingsRow({
  label,
  description,
  children,
}: {
  label: string;
  description?: string;
  children: ReactNode;
}) {
  const { t } = useI18n();
  const helpId = useId();
  const [helpDismissed, setHelpDismissed] = useState(false);
  return (
    <div className="settings-row">
      <span className="settings-row-label">
        <span title={label}>{label}</span>
        {description && (
          <button
            type="button"
            className="settings-help"
            aria-label={t("关于 {setting}", { setting: label })}
            aria-describedby={helpId}
            data-dismissed={helpDismissed || undefined}
            onFocus={() => setHelpDismissed(false)}
            onMouseEnter={() => setHelpDismissed(false)}
            onClick={(event) => event.currentTarget.focus()}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                setHelpDismissed(true);
              }
            }}
          >
            <Info size={14} aria-hidden="true" />
            <span id={helpId} role="tooltip" className="settings-tooltip">
              {description}
            </span>
          </button>
        )}
      </span>
      {children}
    </div>
  );
}
