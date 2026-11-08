import { t, useI18n } from "../i18n";
import { useEffect, useId, useRef, useState } from "react";
import { Check, CircleNotch, Eye, EyeSlash } from "@phosphor-icons/react";
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
  const [noticeOpen, setNoticeOpen] = useState(false);
  const sequence = useRef(0);
  const editor = useRef<HTMLFormElement>(null);
  const noticeId = useId();
  useEffect(() => {
    const request = ++sequence.current;
    setPending("status");
    setStatus(null);
    setDraft("");
    setVisible(false);
    setDirty(false);
    setError("");
    setNoticeOpen(false);
    void bridge
      .deviceKeyStatus(deviceId)
      .then((value) => {
        if (request === sequence.current) setStatus(value);
      })
      .catch((reason) => {
        if (request === sequence.current) {
          setError(errorMessage(reason));
          setNoticeOpen(true);
        }
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
  useEffect(() => {
    if (!noticeOpen) return;
    const outsideClick = (event: MouseEvent) => {
      if (event.target instanceof Node && !editor.current?.contains(event.target)) setNoticeOpen(false);
    };
    const windowBlur = () => setNoticeOpen(false);
    document.addEventListener("click", outsideClick, true);
    window.addEventListener("blur", windowBlur);
    return () => {
      document.removeEventListener("click", outsideClick, true);
      window.removeEventListener("blur", windowBlur);
    };
  }, [noticeOpen]);

  const begin = (operation: Exclude<typeof pending, "">) => {
    const request = ++sequence.current;
    setPending(operation);
    setError("");
    setMessage("");
    setNoticeOpen(false);
    return request;
  };
  const fail = (request: number, reason: unknown) => {
    if (request === sequence.current) {
      setError(errorMessage(reason));
      setNoticeOpen(true);
    }
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
  const showNotice = noticeOpen && !!(error || status?.needsMigration);
  const hint = t(
    status?.needsMigration
      ? "旧设备尚未保存本地密钥。可重新输入，或从旧钥匙串迁移一次。"
      : "保存在本机，默认隐藏；设备连接不再读取系统钥匙串。",
  );
  return (
    <form
      ref={editor}
      className="device-key-editor"
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape" && !event.nativeEvent.isComposing && showNotice) {
          event.preventDefault();
          event.stopPropagation();
          setNoticeOpen(false);
        }
      }}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setNoticeOpen(false);
      }}
    >
      <div className="device-key-input-row">
        <Input
          controlSize="sm"
          type={visible ? "text" : "password"}
          aria-label={t("设备 API key")}
          aria-invalid={!!error || undefined}
          aria-describedby={showNotice ? noticeId : undefined}
          value={draft}
          onFocus={() => setNoticeOpen(!!(error || status?.needsMigration))}
          onClick={() => setNoticeOpen(!!(error || status?.needsMigration))}
          onChange={(event) => {
            setDraft(event.target.value);
            setDirty(true);
            setError("");
            setMessage("");
            setNoticeOpen(false);
          }}
          placeholder={pending === "status" ? t("正在读取密钥状态…") : status?.hasKey ? "••••••••" : "gizclaw_sk_v1_…"}
          title={status?.hasKey ? t("已保存，点眼睛查看或输入新密钥") : undefined}
          autoComplete="off"
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          maxLength={95}
          disabled={blocked}
          readOnly={!status}
        />
        <IconButton
          controlSize="sm"
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
        <IconButton
          controlSize="sm"
          className="device-key-save"
          type="submit"
          aria-label={t("保存")}
          title={message ? t(message) : t("保存")}
          disabled={blocked || !dirty || !draft.trim()}
        >
          {pending === "save" ? <CircleNotch size={14} className="spin" /> : <Check size={14} />}
        </IconButton>
      </div>
      {showNotice && (
        <div className={`device-key-notice${error ? " device-key-error" : ""}`} id={noticeId}>
          <p role={error ? "alert" : undefined}>{error ? t(error) : hint}</p>
          <div
            className="device-key-notice-actions"
            onMouseDown={(event) => {
              // Keep field focus while activating notice actions, including in macOS WebKit.
              if (event.button === 0) event.preventDefault();
            }}
          >
            {error && !status ? (
              <Button
                controlSize="sm"
                variant="ghost"
                disabled={blocked}
                onClick={() => setRetry((value) => value + 1)}
              >
                {t("重新读取")}
              </Button>
            ) : status?.needsMigration ? (
              <Button controlSize="sm" variant="ghost" disabled={blocked} onClick={() => void migrate()}>
                {t("迁移旧密钥")}
              </Button>
            ) : null}
            <Button controlSize="sm" variant="ghost" onClick={() => setNoticeOpen(false)}>
              {t("关闭提示")}
            </Button>
          </div>
        </div>
      )}
      {message && (
        <span className="device-identity-sr" role="status">
          {t(message)}
        </span>
      )}
    </form>
  );
}
