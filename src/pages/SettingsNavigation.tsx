import { useId } from "react";
import { CaretDown, GearSix } from "@phosphor-icons/react";
import { useI18n } from "../i18n";
import "./SettingsNavigation.css";

export type SettingsSection = "general" | "providers" | "service" | "open-source" | "about";

export function SettingsNavigation({
  expanded,
  active,
  onToggle,
  onSelect,
}: {
  expanded: boolean;
  active?: SettingsSection;
  onToggle: () => void;
  onSelect: (section: SettingsSection) => void;
}) {
  const { t } = useI18n();
  const id = useId();
  return (
    <>
      <button
        type="button"
        data-page="settings"
        className={`nav-item settings-toggle${active ? " active" : ""}`}
        aria-label={t("设置")}
        title={t("设置")}
        aria-expanded={expanded}
        aria-controls={expanded ? id : undefined}
        onClick={onToggle}
      >
        <GearSix size={16} />
        <span className="nav-text">{t("设置")}</span>
        <CaretDown size={12} className="settings-caret" aria-hidden="true" />
      </button>
      {expanded && (
        <nav className="settings-navigation" id={id} aria-label={t("设置栏目")}>
          {(
            [
              ["general", "常规"],
              ["providers", "Providers"],
              ["service", "服务"],
              ["open-source", "开源项目"],
              ["about", "关于"],
            ] as const
          ).map(([section, label]) => (
            <button
              key={section}
              type="button"
              className={`settings-link${active === section ? " active" : ""}`}
              data-settings-section={section}
              aria-current={active === section ? "page" : undefined}
              onClick={() => onSelect(section)}
            >
              {t(label)}
            </button>
          ))}
        </nav>
      )}
    </>
  );
}
