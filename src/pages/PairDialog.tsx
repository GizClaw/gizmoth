import { t, useI18n } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { Bluetooth, Broadcast, CaretDown, CaretRight, CircleNotch } from "@phosphor-icons/react";
import { bridge, errorMessage, native, type Device, type Nearby } from "../bridge";
import { Button, Dialog, Input } from "../ui";
import "./PairDialog.css";

type Operation = "scanning" | "binding" | "importing";
type ActiveOperation = { kind: Operation; cancelled: boolean };

export function PairDialog({ onClose, onPaired }: { onClose: () => void; onPaired: (device: Device) => void }) {
  useI18n();
  const [nearby, setNearby] = useState<Nearby[] | null>(null);
  const [url, setUrl] = useState("");
  const [operation, setOperation] = useState<Operation | "idle">("idle");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [showLink, setShowLink] = useState(false);
  const [error, setError] = useState("");
  const active = useRef<ActiveOperation | null>(null);
  const mounted = useRef(false);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const current = active.current;
      if (current && !current.cancelled) {
        current.cancelled = true;
        if (current.kind !== "importing") void bridge.cancelPairing().catch(() => {});
      }
      active.current = null;
    };
  }, []);

  async function run<T>(kind: Operation, action: () => Promise<T>, complete: (result: T) => void) {
    if (active.current) return;
    const current: ActiveOperation = { kind, cancelled: false };
    active.current = current;
    setOperation(kind);
    setError("");
    try {
      const result = await action();
      if (mounted.current && active.current === current && !current.cancelled) {
        active.current = null;
        setOperation("idle");
        setSelectedId(null);
        complete(result);
      }
    } catch (e) {
      if (mounted.current && active.current === current && !current.cancelled) setError(errorMessage(e));
    } finally {
      if (mounted.current && active.current === current && !current.cancelled) {
        active.current = null;
        setOperation("idle");
        setSelectedId(null);
      }
    }
  }

  async function stop() {
    const current = active.current;
    if (!current || current.cancelled || current.kind === "importing") return;
    current.cancelled = true;
    setStopping(true);
    try {
      await bridge.cancelPairing();
    } catch (e) {
      if (mounted.current && active.current === current) setError(errorMessage(e));
    } finally {
      if (mounted.current && active.current === current) {
        active.current = null;
        setStopping(false);
        setOperation("idle");
        setSelectedId(null);
      }
    }
  }

  function close() {
    const current = active.current;
    if (current && !current.cancelled) {
      current.cancelled = true;
      if (current.kind !== "importing") void bridge.cancelPairing().catch(() => {});
    }
    active.current = null;
    onClose();
  }

  const bluetoothBusy = operation === "scanning" || operation === "binding";
  const busy = operation !== "idle";
  return (
    <Dialog title={t("配对设备")} onClose={close} className="pair-dialog">
      <p className="pair-hint">{t("在设备上打开绑定窗口，并开启电脑蓝牙。")}</p>
      <section className="pair-nearby" aria-labelledby="pair-nearby-title">
        <div className="pair-section-head">
          <h3 id="pair-nearby-title">{t("附近的设备")}</h3>
          {bluetoothBusy ? (
            <Button controlSize="sm" onClick={() => void stop()} disabled={stopping}>
              {stopping ? t("正在停止…") : t("停止")}
            </Button>
          ) : (
            <Button
              controlSize="sm"
              variant="primary"
              disabled={busy || !native}
              onClick={() => void run("scanning", bridge.scan, setNearby)}
            >
              <Bluetooth size={14} />
              {nearby ? t("重新查找") : t("查找设备")}
            </Button>
          )}
        </div>
        <div className="pair-device-list" aria-live="polite">
          {nearby?.length ? (
            nearby.map((item) => (
              <button
                className={`pair-device${selectedId === item.id && operation === "binding" ? " is-connecting" : ""}`}
                key={item.id}
                disabled={busy || !native}
                onClick={() => {
                  setSelectedId(item.id);
                  void run("binding", () => bridge.bind(item.id), onPaired);
                }}
              >
                <Broadcast size={17} aria-hidden="true" />
                <span className="pair-device-name" title={item.name}>
                  {item.name}
                </span>
                {selectedId === item.id && operation === "binding" ? (
                  <span className="pair-device-status">
                    <CircleNotch className="spin" size={14} />
                    {t("连接中…")}
                  </span>
                ) : (
                  <>
                    <span className="pair-device-action">{t("连接")}</span>
                    <CaretRight size={14} aria-hidden="true" />
                  </>
                )}
              </button>
            ))
          ) : (
            <div className="pair-empty">
              {operation === "scanning" ? <CircleNotch className="spin" size={19} /> : <Bluetooth size={19} />}
              <span>
                {operation === "scanning"
                  ? t("正在查找附近的设备…")
                  : nearby
                    ? t("没有找到设备，请确认绑定窗口已打开后重试。")
                    : t("查找后，选择要连接的设备。")}
              </span>
            </div>
          )}
        </div>
        {operation === "binding" && <p className="pair-status">{t("正在通过蓝牙读取凭据，请保持设备靠近。")}</p>}
      </section>
      <div className="pair-alternative">
        <button
          className="pair-disclosure"
          type="button"
          aria-expanded={showLink}
          aria-controls="pair-link-form"
          onClick={() => setShowLink(!showLink)}
        >
          {showLink ? <CaretDown size={14} /> : <CaretRight size={14} />}
          {t("使用设备凭据链接")}
        </button>
        {showLink && (
          <form
            id="pair-link-form"
            className="pair-link-form"
            onSubmit={(event) => {
              event.preventDefault();
              void run("importing", () => bridge.importDevice(url.trim()), onPaired);
            }}
          >
            <label htmlFor="credential-url">{t("设备凭据链接")}</label>
            <div className="pair-link-input">
              <Input
                id="credential-url"
                type="password"
                autoComplete="off"
                placeholder="https://…/api-keys/…"
                value={url}
                onChange={(event) => setUrl(event.target.value)}
                disabled={busy || !native}
              />
              <Button type="submit" disabled={busy || !native || !url.trim()}>
                {operation === "importing" ? t("正在添加…") : t("添加")}
              </Button>
            </div>
          </form>
        )}
      </div>
      {!native && <p className="notice">{t("浏览器预览不能访问蓝牙和本机设备存储。")}</p>}
      {error && (
        <p className="error" role="alert">
          {t(error)}
        </p>
      )}
    </Dialog>
  );
}
