import { useId } from "react";
import { ArrowSquareOut, CaretDown, GearSix, Star } from "@phosphor-icons/react";
import { useI18n } from "../i18n";
import { ExternalLink } from "../ExternalLink";
import { gizmothRepository } from "../open-source";
import "./SettingsNavigation.css";

export type SettingsSection = "general" | "open-source";

export function SettingsNavigation({
  expanded,
  active,
  onToggle,
  onSelect,
  onOpenError,
}: {
  expanded: boolean;
  active?: SettingsSection;
  onToggle: () => void;
  onSelect: (section: SettingsSection) => void;
  onOpenError: (error: unknown) => void;
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
              ["open-source", "开源项目"],
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
          <ExternalLink
            className="settings-link settings-star-link"
            href={gizmothRepository}
            onOpenError={onOpenError}
            aria-label={t("在 GitHub 上为 GizMoth 点 Star")}
            title={t("在 GitHub 上为 GizMoth 点 Star")}
          >
            <Star size={14} aria-hidden="true" />
            <span>Star GizMoth</span>
            <ArrowSquareOut size={12} className="settings-external-icon" aria-hidden="true" />
          </ExternalLink>
        </nav>
      )}
    </>
  );
}
