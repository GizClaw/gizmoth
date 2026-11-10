import { useEffect, useState } from "react";
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
  useI18n();
  const snapshot = useSerialSnapshot().snapshot;
  const serviceUpdateError = snapshot?.serviceUpdateError;
  const [confirmRestart, setConfirmRestart] = useState(false);
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [port, setPort] = useState("");
  const [reveal, setReveal] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    if (!native) return;
    void serial
      .gateway()
      .then((value) => {
        setStatus(value);
        setPort(String(value.port));
      })
      .catch((e) => setError(errorMessage(e)));
  }, []);
  const update = (patch: Parameters<typeof serial.updateGateway>[0]) => {
    setBusy(true);
    setError("");
    void serial
      .updateGateway(patch)
      .then((value) => {
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
        setStatus(value);
        setPort(String(value.port));
        await serial.refreshSnapshot();
      })
      .catch((e) => setError(errorMessage(e)))
      .finally(() => setBusy(false));
  };
  const base = status?.urls.at(-1) ?? `http://127.0.0.1:${status?.port ?? 7420}`;
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
              "GizMoth 是每个串口唯一的占据者；本机或局域网的 agent 经由服务查询状态、下发任务或透传数据，不会互相抢占。",
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
                description={t("本机：只有这台电脑上的 agent 可以访问。局域网：同一网络中的其他电脑也可以访问。")}
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
                  {status.urls.map((url, index) => (
                    <SettingsRow key={url} label={index === 0 ? t("本机") : t("局域网")}>
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
                  <dt>GET /v1/ports</dt>
                  <dd>{t("全部串口的连接、模式、占据者、当前任务和 H2Loader 状态。")}</dd>
                  <dt>GET /v1/ports/{"{id}"}/logs?after=</dt>
                  <dd>{t("按游标读取日志。")}</dd>
                  <dt>POST /v1/ports/{"{id}"}/jobs</dt>
                  <dd>{t("接管模式下下发任务：读取状态、安装固件、重启。")}</dd>
                  <dt>PATCH /v1/ports/{"{id}"}</dt>
                  <dd>{t("连接或断开串口；占据策略由 GizMoth 界面管理。")}</dd>
                  <dt>WS /v1/ports/{"{id}"}/pipe?client=&amp;reason=</dt>
                  <dd>
                    {t(
                      "请求占据串口：说明你是谁、为什么需要。排队时收到 queued，获得串口后收到 granted，之后双向转发原始字节；被拒绝或收回时收到 rejected / revoked。断开即释放。",
                    )}
                  </dd>
                </dl>
                <pre className="service-example">{`curl -H "Authorization: Bearer <API Key>" ${base}/v1/ports`}</pre>
              </div>
            </section>
          </>
        ) : (
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
