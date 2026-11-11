import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowsClockwise, CaretRight, Check, Copy, Robot } from "@phosphor-icons/react";
import { agents, type AgentSetup } from "../agents";
import { demo, errorMessage, native } from "../bridge";
import { useI18n } from "../i18n";
import { copyText } from "../clipboard";
import { Button, Dot, IconButton, PageHeader } from "../ui";
import "./Agents.css";

export function AgentsPage({ onSerial, onService }: { onSerial: () => void; onService: () => void }) {
  const { t } = useI18n();
  const [setup, setSetup] = useState<AgentSetup | null>(null);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [feedback, setFeedback] = useState("");
  const [copied, setCopied] = useState(false);
  const request = useRef(0);
  const copyTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const run = useCallback(
    async (key: string, operation: () => Promise<AgentSetup>, success?: (value: AgentSetup) => string | undefined) => {
      const id = ++request.current;
      setBusy(key);
      setError("");
      setFeedback("");
      try {
        const value = await operation();
        if (id !== request.current) return;
        setSetup(value);
        setFeedback(success?.(value) ?? "");
      } catch (e) {
        if (id === request.current) setError(errorMessage(e));
      } finally {
        if (id === request.current) setBusy("");
      }
    },
    [],
  );
  const load = useCallback(() => run("loading", agents.status), [run]);
  useEffect(() => {
    if (native) void load();
    return () => {
      request.current++;
      clearTimeout(copyTimer.current);
    };
  }, [load]);

  function update(id: string, installed: boolean) {
    return run(
      id,
      () => agents.set(id, installed),
      (value) => {
        const target = value.targets.find((item) => item.id === id);
        if (!target || target.error || target.installed !== installed || (installed && !target.current)) return;
        return installed
          ? "Skill 已安装。若 agent 未发现，请重新加载 Skill 或重启 agent。"
          : "GizMoth Skill 已卸载，其他文件已保留。";
      },
    );
  }
  async function copy() {
    if (!setup) return;
    setError("");
    try {
      await copyText(setup.skill);
      setCopied(true);
      clearTimeout(copyTimer.current);
      copyTimer.current = setTimeout(() => setCopied(false), 1800);
    } catch {
      setError(t("无法访问剪贴板，请展开并手动复制 SKILL.md。"));
    }
  }

  const guideRows = [
    {
      heading: "发现串口",
      hint: "本机串口路径、USB 产品名、厂商、VID/PID 和序列号。",
      action: { label: "查看串口", run: onSerial },
    },
    {
      heading: "识别设备",
      hint: "H2Loader 握手后可读取板型、芯片、UID、固件版本和分区。普通串口的 USB 信息不代表所连接设备的型号。",
    },
    {
      heading: "使用串口",
      hint: "MCP/API 查询设备与日志；连接 WS 并等待独占授权后，发送指令、安装固件或重启。",
      action: { label: "服务设置", run: onService },
    },
  ];

  return (
    <>
      <PageHeader crumbs={[{ label: "Agents" }]} />
      <div className="page agents-page" aria-busy={Boolean(busy)}>
        <p className="agents-description">
          {t("安装 GizMoth Skill，让 agent 按需查询串口、识别设备、读取日志和操作硬件。")}
        </p>
        {demo && <p className="notice">{t("演示模式：安装和卸载仅模拟，不会修改本机文件。")}</p>}
        {error && (
          <p className="error banner" role="alert">
            {error}
          </p>
        )}
        <section className="agents-section" aria-labelledby="agents-skill-title">
          <div className="agents-section-head">
            <h2 id="agents-skill-title">GizMoth Skill</h2>
            <IconButton
              controlSize="sm"
              aria-label={t("刷新接入状态")}
              title={t("刷新接入状态")}
              disabled={!native || Boolean(busy)}
              onClick={() => void load()}
            >
              <ArrowsClockwise size={15} className={busy === "loading" ? "spin" : undefined} />
            </IconButton>
          </div>
          {setup ? (
            <ul className="card agents-list" aria-label={t("Agent Skill 列表")}>
              {setup.targets.map((target) => (
                <li className="agents-row" key={target.id}>
                  <Robot size={20} aria-hidden="true" className="agents-glyph" />
                  <div className="agents-identity">
                    <span className="agents-name">{target.name}</span>
                    <code className="agents-path" title={target.path}>
                      {target.path}
                    </code>
                    {target.error && (
                      <>
                        <span className="error" role="alert">
                          {target.error}
                        </span>
                        <span className="agents-recovery">
                          {t("检查 Skill 文件权限或同名冲突，修复后刷新接入状态再重试。")}
                        </span>
                      </>
                    )}
                  </div>
                  <span className="agents-state">
                    <Dot tone={target.error ? "warn" : target.current ? "ok" : "idle"} />
                    {t(
                      target.error ? "无法修改" : target.current ? "已安装" : target.installed ? "需要更新" : "未安装",
                    )}
                  </span>
                  <div className="agents-actions">
                    {target.installed && (
                      <Button
                        controlSize="sm"
                        variant="ghost"
                        aria-label={`${t("卸载 Skill")} · ${target.name}`}
                        disabled={Boolean(busy) || Boolean(target.error)}
                        onClick={() => void update(target.id, false)}
                      >
                        {t("卸载 Skill")}
                      </Button>
                    )}
                    {(!target.current || target.error) && (
                      <Button
                        controlSize="sm"
                        aria-label={`${t(target.installed ? "更新 Skill" : "安装 Skill")} · ${target.name}`}
                        disabled={Boolean(busy) || Boolean(target.error)}
                        onClick={() => void update(target.id, true)}
                      >
                        {busy === target.id ? t("正在安装…") : t(target.installed ? "更新 Skill" : "安装 Skill")}
                      </Button>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          ) : !error && native ? (
            <p className="notice" role="status">
              {t("正在读取接入状态…")}
            </p>
          ) : null}
          <p className="agents-note" role="status">
            {t(feedback || "Skill 独立安装，不修改全局指令。若安装或更新后未被发现，请重新加载 Skill 或重启 agent。")}
          </p>
        </section>
        <section className="agents-guide" aria-labelledby="agents-guide-title">
          <div className="mds-section-heading">
            <h2 id="agents-guide-title">{t("使用说明")}</h2>
          </div>
          <div className="mds-group">
            {setup && (
              <details className="agents-skill">
                <summary className="mds-row mds-row--toggle">
                  <span className="mds-row-label">SKILL.md</span>
                  <span className="mds-row-control">
                    <span className="agents-disclosure-label">{t("查看完整指令")}</span>
                    <CaretRight size={14} aria-hidden="true" />
                  </span>
                </summary>
                <div className="agents-skill-body">
                  <div className="agents-skill-toolbar">
                    <span>{t("可复制到其他支持 Skill 的 agent。文件不包含 API Key。")}</span>
                    <button type="button" className="mds-link-button" onClick={() => void copy()}>
                      {copied ? <Check size={14} /> : <Copy size={14} />}
                      {t(copied ? "已复制" : "复制 SKILL.md")}
                    </button>
                  </div>
                  <pre className="mds-pre" tabIndex={0} aria-label={t("GizMoth Skill 内容")}>
                    {setup.skill}
                  </pre>
                </div>
              </details>
            )}
            {guideRows.map(({ heading, hint, action }) => (
              <div className="mds-row" key={heading}>
                <div className="mds-row-label">
                  <h3>{t(heading)}</h3>
                </div>
                <div className="mds-row-control agents-guide-control">
                  <p className="mds-hint">{t(hint)}</p>
                  {action && (
                    <button type="button" className="mds-link-button" onClick={action.run}>
                      {t(action.label)}
                      <CaretRight size={14} aria-hidden="true" />
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>
        </section>
      </div>
    </>
  );
}
