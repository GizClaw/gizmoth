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
  const [certificate, setCertificate] = useState("");
  const [privateKey, setPrivateKey] = useState("");
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
        setCertificate((draft) =>
          !previous || draft === (previous.tlsCertificateChain ?? "") ? (value.tlsCertificateChain ?? "") : draft,
        );
        setPrivateKey((draft) =>
          !previous || draft === (previous.tlsPrivateKey ?? "") ? (value.tlsPrivateKey ?? "") : draft,
        );
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
        setCertificate(value.tlsCertificateChain ?? "");
        setPrivateKey(value.tlsPrivateKey ?? "");
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
        setCertificate(value.tlsCertificateChain ?? "");
        setPrivateKey(value.tlsPrivateKey ?? "");
        await serial.refreshSnapshot();
      })
      .catch((e) => setError(errorMessage(e)))
      .finally(() => setBusy(false));
  };
  const urls = (status?.urls ?? []).filter((value) => {
    try {
      const url = new URL(value);
      return url.protocol === "https:" || (url.protocol === "http:" && url.hostname === "127.0.0.1");
    } catch {
      return false;
    }
  });
  const unsafeUrls = status && urls.length !== status.urls.length;
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
                ? "Clients using GizMoth share serial access through the service. Local agents use HTTP; LAN agents use HTTPS and WSS with certificate verification."
                : "通过 GizMoth 的客户端由服务统一管理串口访问。本机 agent 使用 HTTP；局域网 agent 使用 HTTPS 和 WSS，并校验证书。",
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
        {unsafeUrls && (
          <div className="service-update-error">
            <p className="error banner" role="alert">
              {language === "en"
                ? "The running service reported an insecure LAN address. Restart the service before using its API key."
                : "当前服务报告了不安全的局域网地址。使用 API Key 前请重启服务。"}
            </p>
            <Button controlSize="sm" disabled={busy || hasDeviceWork} onClick={() => setConfirmRestart(true)}>
              {t("重新启动服务")}
            </Button>
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
                    ? "Local agents use loopback HTTP. LAN requires TLS files and a certificate trusted by clients."
                    : "本机 agent 使用 loopback HTTP。局域网需要配置 TLS 文件，且客户端必须信任并校验证书。"
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
            <section className="settings-group">
              <h2>{language === "en" ? "LAN TLS" : "局域网 TLS"}</h2>
              <form
                className="card settings-list service-tls"
                onSubmit={(event) => {
                  event.preventDefault();
                  update({ tlsCertificateChain: certificate, tlsPrivateKey: privateKey });
                }}
              >
                <SettingsRow
                  label={language === "en" ? "Certificate chain" : "证书链"}
                  description={
                    language === "en"
                      ? "Absolute path to a PEM file containing the server certificate followed by intermediates. Clients must verify the chain and hostname."
                      : "服务器证书及中间证书的 PEM 文件绝对路径。客户端必须校验证书链与主机名。"
                  }
                >
                  <Input
                    className="service-tls-path"
                    controlSize="sm"
                    aria-label={language === "en" ? "Certificate chain path" : "证书链路径"}
                    autoComplete="off"
                    spellCheck={false}
                    value={certificate}
                    disabled={busy}
                    onChange={(event) => setCertificate(event.target.value)}
                  />
                </SettingsRow>
                <SettingsRow
                  label={language === "en" ? "Private key" : "私钥"}
                  description={
                    language === "en"
                      ? "Absolute path to the matching unencrypted PEM private key. Only the path is saved; the service reads the file locally."
                      : "匹配的未加密 PEM 私钥文件绝对路径。仅保存路径，由服务在本机读取文件。"
                  }
                >
                  <Input
                    className="service-tls-path"
                    controlSize="sm"
                    aria-label={language === "en" ? "Private key path" : "私钥路径"}
                    autoComplete="off"
                    spellCheck={false}
                    value={privateKey}
                    disabled={busy}
                    onChange={(event) => setPrivateKey(event.target.value)}
                  />
                </SettingsRow>
                <div className="settings-search-footer">
                  <span className="settings-search-message">
                    {language === "en"
                      ? "Save to reload certificates. Invalid TLS files keep the current listeners."
                      : "保存会重新加载证书；TLS 文件无效时保留当前监听配置。"}
                  </span>
                  <Button controlSize="sm" type="submit" disabled={busy}>
                    {t("保存")}
                  </Button>
                </div>
              </form>
            </section>
            {status.listening && (
              <section className="settings-group">
                <h2>{t("访问地址")}</h2>
                <div className="card settings-list">
                  {urls.map((url, index) => (
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
                  <dt>GET /v1/system-ports</dt>
                  <dd>{t("本机串口路径、USB 产品名、厂商、VID/PID 和序列号。")}</dd>
                  <dt>GET /v1/ports</dt>
                  <dd>{t("全部串口的连接、模式、占据者、当前任务和 H2Loader 状态。")}</dd>
                  <dt>GET /v1/ports/{"{id}"}</dt>
                  <dd>
                    {t(
                      "H2Loader 握手后可读取板型、芯片、UID、固件版本和分区。普通串口的 USB 信息不代表所连接设备的型号。",
                    )}
                  </dd>
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
          native && !error && (
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
