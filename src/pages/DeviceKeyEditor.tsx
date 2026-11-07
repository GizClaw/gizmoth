import { t, useI18n } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { ArrowClockwise, CircleNotch, Eye, EyeSlash, Key } from "@phosphor-icons/react";
import { bridge, errorMessage, type DeviceKeyStatus } from "../bridge";
import { Button, IconButton, Input } from "../ui";

export function DeviceKeyEditor({
  deviceId,
  disabled,
  onSaved,
}: {
  deviceId: string;
  disabled: boolean;
  onSaved: () => void;
}) {
  useI18n();
  const [status, setStatus] = useState<DeviceKeyStatus | null>(null);
  const [draft, setDraft] = useState("");
  const [visible, setVisible] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [pending, setPending] = useState<"status" | "reveal" | "save" | "migrate" | "">("status");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [retry, setRetry] = useState(0);
  const sequence = useRef(0);
  useEffect(() => {
    const request = ++sequence.current;
    setPending("status");
    setStatus(null);
    setDraft("");
    setVisible(false);
    setDirty(false);
    setError("");
    void bridge
      .deviceKeyStatus(deviceId)
      .then((value) => {
        if (request === sequence.current) setStatus(value);
      })
      .catch((reason) => {
        if (request === sequence.current) setError(errorMessage(reason));
      })
      .finally(() => {
        if (request === sequence.current) setPending("");
      });
    return () => {
      sequence.current++;
    };
  }, [deviceId, retry]);
  useEffect(() => {
    const hide = () => {
      if (document.hidden) setVisible(false);
    };
    document.addEventListener("visibilitychange", hide);
    return () => document.removeEventListener("visibilitychange", hide);
  }, []);

  const begin = (operation: Exclude<typeof pending, "">) => {
    const request = ++sequence.current;
    setPending(operation);
    setError("");
    setMessage("");
    return request;
  };
  const fail = (request: number, reason: unknown) => {
    if (request === sequence.current) setError(errorMessage(reason));
  };
  const done = (request: number) => {
    if (request === sequence.current) setPending("");
  };
  async function toggleVisibility() {
    if (visible || draft) {
      setVisible((value) => !value);
      return;
    }
    if (!status?.hasKey) return;
    const request = begin("reveal");
    try {
      const key = await bridge.deviceKey(deviceId);
      if (request !== sequence.current) return;
      if (key === null) {
        setStatus({ hasKey: false, needsMigration: true });
        return;
      }
      setDraft(key);
      setVisible(true);
      setDirty(false);
    } catch (reason) {
      fail(request, reason);
    } finally {
      done(request);
    }
  }
  async function save() {
    if (disabled || pending || !dirty || !draft.trim()) return;
    const request = begin("save");
    try {
      await bridge.saveDeviceKey(deviceId, draft.trim());
      if (request !== sequence.current) return;
      setDraft("");
      setVisible(false);
      setDirty(false);
      setStatus({ hasKey: true, needsMigration: false });
      setMessage("设备 API key 已保存在本机。");
      onSaved();
    } catch (reason) {
      fail(request, reason);
    } finally {
      done(request);
    }
  }
  async function migrate() {
    if (disabled || pending) return;
    const request = begin("migrate");
    try {
      const result = await bridge.migrateDeviceKey(deviceId);
      if (request !== sequence.current) return;
      setDraft("");
      setVisible(false);
      setDirty(false);
      setStatus({ hasKey: true, needsMigration: false });
      setMessage(result.legacyRemoved ? "已迁移到本机，并移除旧钥匙串副本。" : "本机已保存密钥；旧钥匙串副本未移除。");
      onSaved();
    } catch (reason) {
      fail(request, reason);
    } finally {
      done(request);
    }
  }
  const blocked = disabled || !!pending;
  return (
    <form
      className="device-key-editor"
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <div className="device-key-input-row">
        <Input
          type={visible ? "text" : "password"}
          aria-label={t("设备 API key")}
          value={draft}
          onChange={(event) => {
            setDraft(event.target.value);
            setDirty(true);
            setError("");
            setMessage("");
          }}
          placeholder={
            pending === "status"
              ? t("正在读取密钥状态…")
              : status?.hasKey
                ? t("已保存，点眼睛查看或输入新密钥")
                : "gizclaw_sk_v1_…"
          }
          autoComplete="off"
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          maxLength={95}
          disabled={blocked || !status}
        />
        <IconButton
          controlSize="md"
          aria-label={visible ? t("隐藏设备 API key") : t("显示设备 API key")}
          title={visible ? t("隐藏设备 API key") : t("显示设备 API key")}
          aria-pressed={visible}
          disabled={blocked || (!draft && !status?.hasKey)}
          onClick={() => void toggleVisibility()}
        >
          {pending === "reveal" ? (
            <CircleNotch size={16} className="spin" />
          ) : visible ? (
            <EyeSlash size={16} />
          ) : (
            <Eye size={16} />
          )}
        </IconButton>
        <Button type="submit" disabled={blocked || !dirty || !draft.trim()}>
          {pending === "save" && <CircleNotch size={14} className="spin" />}
          {t("保存")}
        </Button>
      </div>
      {status?.needsMigration ? (
        <div className="device-key-migration">
          <p>{t("旧设备尚未保存本地密钥。可重新输入，或从旧钥匙串迁移一次。")}</p>
          <Button controlSize="sm" disabled={blocked} onClick={() => void migrate()}>
            {pending === "migrate" ? <CircleNotch size={14} className="spin" /> : <Key size={14} />}
            {t("迁移旧密钥")}
          </Button>
        </div>
      ) : (
        <p className="device-key-hint">{t("保存在本机，默认隐藏；设备连接不再读取系统钥匙串。")}</p>
      )}
      {error && (
        <p className="device-key-error" role="alert">
          {t(error)}
        </p>
      )}
      {error && !status && (
        <Button controlSize="sm" disabled={blocked} onClick={() => setRetry((value) => value + 1)}>
          <ArrowClockwise size={14} />
          {t("重新读取")}
        </Button>
      )}
      {message && (
        <p className="device-key-feedback" role="status">
          {t(message)}
        </p>
      )}
    </form>
  );
}
