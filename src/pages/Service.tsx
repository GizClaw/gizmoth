import { useEffect, useRef, useState } from "react";
import { ArrowsClockwise, Copy, Eye, EyeSlash } from "@phosphor-icons/react";
import { t, useI18n } from "../i18n";
import { errorMessage, native } from "../bridge";
import { Button, Dialog, Dot, IconButton, Input, PageHeader, Select } from "../ui";
import { serial, useSerialSnapshot, type GatewayStatus } from "../serial";
import { SettingsRow } from "./SettingsRow";
import "./Settings.css";
import "./Service.css";

function CopyButton({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <IconButton
      aria-label={label}
      title={copied ? t("已复制") : label}
      onClick={() =>
        void navigator.clipboard?.writeText(value).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        })
      }
    >
      <Copy size={15} />
    </IconButton>
  );
}

/** Settings › Service: GizMoth's single HTTP/WebSocket service for local or LAN agents. */
export function ServicePage() {
  const { language } = useI18n();
  const snapshot = useSerialSnapshot().snapshot;
  const serviceUpdateError = snapshot?.serviceUpdateError;
  const [confirmRestart, setConfirmRestart] = useState(false);
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [port, setPort] = useState("");
  const [reveal, setReveal] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const loadedStatus = useRef<GatewayStatus | null>(null);
  // The watch summary contains only public transport state, so address changes
  // refresh this page without polling secrets or reacting to every log update.
  const gatewayState = JSON.stringify(snapshot?.gateway);
  useEffect(() => {
    if (!native || busy) return;
    let cancelled = false;
    void serial
      .gateway()
      .then((value) => {
        if (cancelled) return;
        const previous = loadedStatus.current;
        loadedStatus.current = value;
        setStatus(value);
        setPort((draft) => (!previous || draft === String(previous.port) ? String(value.port) : draft));
      })
      .catch((e) => {
        if (!cancelled) setError(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [gatewayState, busy]);
  const update = (patch: Parameters<typeof serial.updateGateway>[0]) => {
    setBusy(true);
    setError("");
    void serial
      .updateGateway(patch)
      .then((value) => {
        loadedStatus.current = value;
        setStatus(value);
        setPort(String(value.port));
      })
      .catch((e) => setError(errorMessage(e)))
      .finally(() => setBusy(false));
  };
  const hasDeviceWork = snapshot?.ports.some(
    (item) => item.preparing || item.job || item.holder?.kind === "client" || item.queue?.length,
  );
  const restart = () => {
    setConfirmRestart(false);
    setBusy(true);
    setError("");
    void serial
      .stopService()
      .then(() => serial.gateway())
      .then(async (value) => {
        loadedStatus.current = value;
        setStatus(value);
        setPort(String(value.port));
        await serial.refreshSnapshot();
      })
      .catch((e) => setError(errorMessage(e)))
      .finally(() => setBusy(false));
  };
  const urls = status?.urls ?? [];
  const base = urls.at(-1) ?? `http://127.0.0.1:${status?.port ?? 7420}`;
  const key = status?.apiKey ?? "";
  const masked = key ? `${key.slice(0, 8)}${"•".repeat(20)}${key.slice(-4)}` : "";
  const portChanged = status && port !== String(status.port);
  return (
    <>
      <PageHeader crumbs={[{ label: t("设置") }, { label: t("服务") }]} />
      <div className="page settings-page service-page">
        <div className="page-description">
          <p>{t("让 agent 通过 GizMoth 使用这台电脑上的串口。")}</p>
          <span>
            {t(
              language === "en"
                ? "GizMoth shares serial access over HTTP, WebSocket and gRPC (h2c). Connect on your LAN or Tailscale with an address and API key; no certificates are needed."
                : "通过 HTTP、WebSocket 和 gRPC（h2c）共享串口。局域网或 Tailscale 内使用地址和 API Key 连接，无需证书。",
            )}
          </span>
        </div>
        {(error || serviceUpdateError) && (
          <div className="service-update-error">
            <p className="error banner" role="alert">
              {t(error || serviceUpdateError || "")}
            </p>
            {serviceUpdateError && (
              <Button controlSize="sm" disabled={busy || hasDeviceWork} onClick={() => setConfirmRestart(true)}>
                {t("重新启动服务")}
              </Button>
            )}
          </div>
        )}
        {status ? (
          <>
            <section className="card settings-list" aria-label={t("服务")}>
              <SettingsRow label={t("启用服务")}>
                <span className="service-state">
                  <Dot tone={status.error ? "warn" : status.listening ? "ok" : "off"} />
                  {status.error ? t(status.error) : status.listening ? t("正在提供服务") : t("已关闭")}
                </span>
                <input
                  type="checkbox"
                  role="switch"
                  className="switch"
                  aria-label={t("启用服务")}
                  checked={status.enabled}
                  disabled={busy}
                  onChange={(event) => update({ enabled: event.target.checked })}
                />
              </SettingsRow>
              <SettingsRow
                label={t("服务范围")}
                description={
                  language === "en"
                    ? "Local limits access to this computer. LAN also allows connections through LAN and Tailscale addresses."
                    : "本机仅允许此电脑访问；局域网同时开放 LAN 和 Tailscale 地址。"
                }
              >
                <Select
                  className="settings-preference-select"
                  controlSize="sm"
                  aria-label={t("服务范围")}
                  disabled={busy || !status.enabled}
                  value={status.scope}
                  onValueChange={(value) => update({ scope: value as "local" | "lan" })}
                  options={[
                    { value: "local", label: t("本机") },
                    { value: "lan", label: t("局域网") },
                  ]}
                />
              </SettingsRow>
              <SettingsRow label={t("端口")}>
                <form
                  className="service-port"
                  onSubmit={(event) => {
                    event.preventDefault();
                    const value = Number(port);
                    if (Number.isInteger(value)) update({ port: value });
                  }}
                >
                  <Input
                    controlSize="sm"
                    inputMode="numeric"
                    aria-label={t("端口")}
                    value={port}
                    disabled={busy}
                    onChange={(event) => setPort(event.target.value.replace(/\D/g, "").slice(0, 5))}
                  />
                  {portChanged && (
                    <Button controlSize="sm" type="submit" disabled={busy}>
                      {t("应用")}
                    </Button>
                  )}
                </form>
              </SettingsRow>
              <SettingsRow
                label="API Key"
                description={t("agent 以 Authorization: Bearer <API Key> 访问服务。重新生成后旧的 Key 立即失效。")}
              >
                <span className="service-key">
                  <code>{reveal ? key : masked}</code>
                  <IconButton
                    aria-label={reveal ? t("隐藏") : t("显示")}
                    title={reveal ? t("隐藏") : t("显示")}
                    onClick={() => setReveal((value) => !value)}
                  >
                    {reveal ? <EyeSlash size={15} /> : <Eye size={15} />}
                  </IconButton>
                  <CopyButton value={key} label={t("复制 API Key")} />
                  <IconButton
                    aria-label={t("重新生成")}
                    title={t("重新生成")}
                    disabled={busy}
                    onClick={() => update({ regenerateKey: true })}
                  >
                    <ArrowsClockwise size={15} />
                  </IconButton>
                </span>
              </SettingsRow>
            </section>
            {status.listening && (
              <section className="settings-group">
                <h2>{t("访问地址")}</h2>
                <div className="card settings-list">
                  {urls.map((url, index) => (
                    <SettingsRow
                      key={url}
                      label={
                        index === 0
                          ? t("本机")
                          : /^http:\/\/100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./.test(url)
                            ? "Tailscale"
                            : t("局域网")
                      }
                    >
                      <span className="service-key">
                        <code>{url}</code>
                        <CopyButton value={url} label={t("复制地址")} />
                      </span>
                    </SettingsRow>
                  ))}
                </div>
              </section>
            )}
            <section className="settings-group">
              <h2>{t("接口")}</h2>
              <div className="card service-api">
                <dl>
                  <dt>GET /v1/system-ports</dt>
                  <dd>{t("本机串口路径、USB 产品名、厂商、VID/PID 和序列号。")}</dd>
                  <dt>GET /v1/ports</dt>
                  <dd>{t("全部串口的连接、通信模式、通信方、当前任务和 H2Loader 状态。")}</dd>
                  <dt>GET /v1/ports/{"{id}"}</dt>
                  <dd>
                    {t(
                      "H2Loader 握手后可读取板型、芯片、UID、固件版本和分区。普通串口的 USB 信息不代表所连接设备的型号。",
                    )}
                  </dd>
                  <dt>GET /v1/ports/{"{id}"}/logs?after=</dt>
                  <dd>{t("按游标读取日志。")}</dd>
                  <dt>WS /v1/ports/{"{id}"}/pipe?client=&amp;reason=</dt>
                  <dd>
                    {t(
                      "连接即申请独占，默认自动批准。等待时保持 pending，收到 granted 后才能下发命令。断开即释放，界面和其他发送入口在独占期间暂停。串口及设备协议始终由 GizMoth 管理。",
                    )}
                  </dd>
                  <dt>WS: send · install · reboot · refresh · cancel</dt>
                  <dd>
                    {t(
                      "获批后发送 JSON 命令，使用 id 关联 result；安装、重启和刷新通过 job 事件报告进度与结果。MCP/API 仅提供查询和 WS 连接信息。",
                    )}
                  </dd>
                  <dt>gRPC gizmoth.gateway.v1</dt>
                  <dd>
                    {t(
                      "GizMoth 之间的串口级联，与上述接口同一端口（HTTP/2）：节点信息、串口拓扑订阅、日志、独占会话、固件上传和任务查询。只导出本机串口。",
                    )}
                  </dd>
                </dl>
                <pre className="service-example">{`curl -H "Authorization: Bearer <API Key>" ${base}/v1/ports`}</pre>
              </div>
            </section>
          </>
        ) : (
          native &&
          !error && (
            <p className="notice" role="status">
              {t("正在启动服务…")}
            </p>
          )
        )}
      </div>
      {confirmRestart && (
        <Dialog title={t("重新启动服务")} onClose={() => setConfirmRestart(false)}>
          <p className="dialog-text">{t("重启会断开串口连接，再由当前版本重新连接已开启的串口。")}</p>
          <div className="dialog-actions">
            <Button onClick={() => setConfirmRestart(false)}>{t("取消")}</Button>
            <Button variant="primary" disabled={busy || hasDeviceWork} onClick={restart}>
              {t("重新启动服务")}
            </Button>
          </div>
        </Dialog>
      )}
    </>
  );
}
