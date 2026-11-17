import { useEffect, useState } from "react";
import { Check, Copy } from "@phosphor-icons/react";
import { compile, type CompileTaskConfig } from "../compile";
import { errorMessage } from "../bridge";
import { copyText } from "../clipboard";
import { useI18n } from "../i18n";
import { Button, IconButton } from "../ui";
import { JsonCode } from "../JsonCode";

/** Fetch the accepted request once, separately from live status snapshots. */
export function TaskConfiguration({ jobId }: { jobId: string }) {
  const { t } = useI18n();
  const [config, setConfig] = useState<CompileTaskConfig | null>(null);
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState("");
  useEffect(() => {
    let disposed = false;
    setConfig(null);
    setError("");
    void compile.jobConfig(jobId).then((value) => {
      if (!disposed) setConfig(value);
    }).catch((reason) => {
      if (!disposed) setError(errorMessage(reason));
    });
    return () => { disposed = true; };
  }, [jobId, attempt]);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1800);
    return () => clearTimeout(timer);
  }, [copied]);
  const text = config ? `${JSON.stringify(config, null, 2)}\n` : "";
  return (
    <section className="compile-config-wrap" aria-label={t("任务配置")} aria-busy={!config && !error}>
      <div className="compile-config-toolbar">
        <span className="compile-config-filename">gizmoth-task.json</span>
        <IconButton aria-label={t("复制配置")} title={t("复制配置")} disabled={!config} onClick={() => {
          setCopyError("");
          void copyText(text).then(() => setCopied(true)).catch((reason) => setCopyError(errorMessage(reason)));
        }}>{copied ? <Check size={15} /> : <Copy size={15} />}</IconButton>
        <span className="compile-sr-only" role="status">{copied ? t("已复制配置") : ""}</span>
      </div>
      {config ? (
        <pre className="compile-config-code" tabIndex={0} aria-label={t("任务配置 JSON")}><JsonCode text={text} /></pre>
      ) : error ? (
        <div className="compile-config-message">
          <p className="error" role="alert">{t("无法读取任务配置：{error}", { error })}</p>
          <Button controlSize="sm" onClick={() => setAttempt((value) => value + 1)}>{t("重试")}</Button>
        </div>
      ) : <p className="compile-config-message">{t("正在读取任务配置…")}</p>}
      {copyError && <p className="error compile-config-copy-error" role="alert">{copyError}</p>}
    </section>
  );
}
