import { useState } from "react";
import { GithubLogo, Star } from "@phosphor-icons/react";
import { PageHeader } from "../ui";
import { useI18n } from "../i18n";
import { ExternalLink } from "../ExternalLink";
import { errorMessage } from "../bridge";
import { openSourceProjects, projectRepository } from "../open-source";
import "./OpenSource.css";

export function OpenSourcePage({ onGeneral }: { onGeneral: () => void }) {
  const { t } = useI18n();
  const [error, setError] = useState("");
  const failed = (error: unknown) => setError(errorMessage(error));
  return <>
    <PageHeader crumbs={[{ label: t("设置"), onClick: onGeneral }, { label: t("开源项目") }]} />
    <div className="page narrow open-source-page">
      <p className="open-source-intro">{t("探索 GizClaw 的开源项目，查看源码、阅读文档，或参与贡献。")}</p>
      {error && <p className="error banner" role="alert">{t("无法打开链接：{error}", { error })}</p>}
      <section className="open-source-list" aria-label={t("GizClaw 开源项目")}>
        {openSourceProjects.map(project => <article className="open-source-project" key={project.repository}>
          <ExternalLink className="open-source-project-link" href={projectRepository(project.repository)} onClick={() => setError("")} onOpenError={failed} aria-label={t("在 GitHub 打开 {name}", { name: project.name })} title={projectRepository(project.repository)}>
            <GithubLogo size={19} className="open-source-project-icon" aria-hidden="true" />
            <span className="open-source-project-copy">
              <span className="open-source-project-heading"><strong>{project.name}</strong><span>GizClaw/{project.repository}</span></span>
              <span className="open-source-project-description">{t(project.description)}</span>
            </span>
          </ExternalLink>
          <ExternalLink className="button ghost control-sm open-source-star" href={projectRepository(project.repository)} onClick={() => setError("")} onOpenError={failed} aria-label={t("在 GitHub 上为 {name} 点 Star", { name: project.name })} title={t("在 GitHub 上为 {name} 点 Star", { name: project.name })}>
            <Star size={14} aria-hidden="true" />
            <span>Star</span>
          </ExternalLink>
        </article>)}
      </section>
      <p className="open-source-note">{t("项目链接将在系统浏览器中打开。")}</p>
    </div>
  </>;
}
