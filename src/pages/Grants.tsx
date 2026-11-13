import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowsClockwise, Spinner } from "@phosphor-icons/react";
import { demo, errorMessage, native } from "../bridge";
import {
  grants,
  type NotificationSettings,
  type PermissionState,
  type SystemPermission,
  type SystemPermissions,
} from "../grants";
import { useI18n } from "../i18n";
import { Button, IconButton, PageHeader } from "../ui";
import { SettingsRow } from "./SettingsRow";
import "./Settings.css";
import "./Grants.css";

const permissions = [
  { id: "notifications", label: "系统通知", help: "串口任务完成、失败或取消时，通过系统通知提醒你。" },
  { id: "bluetooth", label: "蓝牙", help: "发现附近的 GizClaw 设备并进行配对。授权不会开始扫描或连接设备。" },
  {
    id: "localNetwork",
    label: "本地网络",
    help: "连接局域网中的设备和其他 GizMoth。本权限由系统在首次连接时询问，无法直接读取授权状态。",
  },
] as const;
const stateLabels: Record<PermissionState, string> = {
  granted: "已授权",
  denied: "未授权",
  prompt: "待授权",
  unknown: "由系统管理",
  unavailable: "不可用",
};

export function GrantsPage() {
  const { t } = useI18n();
  const [states, setStates] = useState<SystemPermissions | null>(null);
  const [settings, setSettings] = useState<NotificationSettings | null>(null);
  const [loading, setLoading] = useState(native);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const inFlight = useRef(false);
  const mounted = useRef(false);
  const request = useRef(0);
  const refresh = useCallback(async () => {
    if (!native) return;
    const sequence = ++request.current;
    try {
      const [permissions, notifications] = await Promise.all([grants.permissions(), grants.notifications()]);
      if (!mounted.current || sequence !== request.current) return;
      setStates(permissions);
      setSettings(notifications);
      setError("");
    } catch (e) {
      if (mounted.current && sequence === request.current) setError(errorMessage(e));
    } finally {
      if (mounted.current && sequence === request.current) setLoading(false);
    }
  }, []);
  useEffect(() => {
    mounted.current = true;
    void refresh();
    const onFocus = () => {
      if (!inFlight.current && document.visibilityState === "visible") void refresh();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      mounted.current = false;
      request.current++;
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    };
  }, [refresh]);

  async function act(id: string, action: () => Promise<void>) {
    if (inFlight.current) return;
    inFlight.current = true;
    request.current++;
    setBusy(id);
    setError("");
    setMessage("");
    try {
      await action();
      await refresh();
    } catch (e) {
      if (mounted.current) setError(errorMessage(e));
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy("");
    }
  }
  const pending = permissions.filter(({ id }) => id !== "localNetwork" && states?.[id] === "prompt");
  const disabled = !native || loading || !!busy;
  const authorize = async (permission: Exclude<SystemPermission, "localNetwork">) => {
    const state = await grants.request(permission);
    if (mounted.current) setStates((current) => current && { ...current, [permission]: state });
  };
  return (
    <>
      <PageHeader crumbs={[{ label: t("设置") }, { label: t("授权") }]} />
      <div className="page settings-page grants-page">
        <div className="grants-intro">
          <p>{t("管理 GizMoth 使用的系统权限。你可以随时在系统设置中更改授权。")}</p>
          <IconButton title={t("刷新授权状态")} disabled={disabled} onClick={() => void act("refresh", refresh)}>
            <ArrowsClockwise size={15} className={busy === "refresh" ? "spin" : undefined} />
          </IconButton>
        </div>
        {demo && <p className="grants-note">{t("演示中的授权和通知仅为模拟，不会修改系统权限或发送通知。")}</p>}
        {error && (
          <p className="error banner" role="alert">
            {t(error)}
          </p>
        )}
        {loading && (
          <p className="grants-note" role="status">
            <Spinner size={14} className="spin" />
            {t("正在读取授权状态…")}
          </p>
        )}
        <section className="settings-group" aria-labelledby="system-grants-heading">
          <div className="grants-group-heading">
            <h2 id="system-grants-heading">{t("系统权限")}</h2>
            {pending.length > 0 && (
              <Button
                controlSize="sm"
                variant="ghost"
                disabled={disabled}
                onClick={() =>
                  void act("all", async () => {
                    for (const { id } of pending) if (id !== "localNetwork") await authorize(id);
                  })
                }
              >
                {busy === "all" && <Spinner size={13} className="spin" />}
                {t("请求所需授权")}
              </Button>
            )}
          </div>
          <div className="card settings-list">
            {permissions.map(({ id, label, help }) => {
              const state = states?.[id] ?? "unavailable";
              const canRequest = id !== "localNetwork" && state === "prompt";
              return (
                <SettingsRow key={id} label={t(label)} description={t(help)}>
                  <span className="grant-state" data-state={state} role="status">
                    {states ? t(stateLabels[state]) : native ? "—" : t("不可用")}
                  </span>
                  <Button
                    controlSize="sm"
                    disabled={disabled || !states || state === "unavailable"}
                    aria-label={t(canRequest ? "授权{permission}" : "打开{permission}系统设置", {
                      permission: t(label),
                    })}
                    onClick={() =>
                      void act(id, async () => {
                        if (canRequest) await authorize(id);
                        else await grants.openSettings(id);
                      })
                    }
                  >
                    {busy === id && <Spinner size={13} className="spin" />}
                    {t(canRequest ? "授权" : "系统设置")}
                  </Button>
                </SettingsRow>
              );
            })}
          </div>
        </section>
        <section className="settings-group" aria-labelledby="notification-grants-heading">
          <h2 id="notification-grants-heading">{t("通知")}</h2>
          <div className="card settings-list">
            <SettingsRow
              label={t("串口任务通知")}
              description={t(
                "提醒固件安装、Stage 传输和重启的最终结果，包括外部客户端发起的任务。状态刷新不会发送通知。",
              )}
            >
              <input
                type="checkbox"
                role="switch"
                className="switch"
                aria-label={t("串口任务通知")}
                checked={settings?.serialTasks ?? false}
                disabled={disabled || !settings}
                onChange={(event) => {
                  const enabled = event.target.checked;
                  void act("serial", () => grants.setNotifications(enabled));
                }}
              />
            </SettingsRow>
            <div className="settings-search-footer">
              <span className="settings-search-message" role="status">
                {message
                  ? t(message)
                  : states?.notifications !== "granted"
                    ? t("允许系统通知后，串口任务提醒才会送达。")
                    : t("收起窗口后仍会提醒；完全退出 GizMoth 后不再发送。")}
              </span>
              <Button
                controlSize="sm"
                disabled={disabled || states?.notifications !== "granted"}
                onClick={() =>
                  void act("test", async () => {
                    await grants.testNotification();
                    if (mounted.current) setMessage("测试通知已提交，请在系统通知中心查看。");
                  })
                }
              >
                {busy === "test" && <Spinner size={13} className="spin" />}
                {t("发送测试通知")}
              </Button>
            </div>
          </div>
        </section>
      </div>
    </>
  );
}
