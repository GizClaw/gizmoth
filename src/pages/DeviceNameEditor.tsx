import { useId, useRef, useState } from "react";
import { Check, CircleNotch, PencilSimple, WarningCircle, X } from "@phosphor-icons/react";
import { bridge, errorMessage, native, type Device } from "../bridge";
import { t, useI18n } from "../i18n";
import { IconButton, Input } from "../ui";

export function DeviceNameEditor({
  device,
  headingId,
  disabled,
  onSaved,
}: {
  device: Device;
  headingId: string;
  disabled: boolean;
  onSaved: (device: Device) => void;
}) {
  useI18n();
  const errorId = useId();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(device.name);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const trigger = useRef<HTMLButtonElement>(null);
  const saving = useRef(false);
  const blocked = disabled || pending || !native;
  const valid = !!draft.trim() && [...draft.trim()].length <= 40;
  const close = () => {
    setEditing(false);
    requestAnimationFrame(() => trigger.current?.focus());
  };
  async function save() {
    if (blocked || saving.current || !valid) return;
    if (draft.trim() === device.name) return close();
    saving.current = true;
    setPending(true);
    setError("");
    try {
      onSaved(await bridge.renameDevice(device.id, draft.trim()));
      close();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      saving.current = false;
      setPending(false);
    }
  }
  return (
    <div className="device-name-editor">
      <h1 id={headingId} title={device.name} className={editing ? "device-identity-sr" : undefined}>
        {device.name}
      </h1>
      {editing ? (
        <form
          className="device-name-form"
          onSubmit={(event) => {
            event.preventDefault();
            void save();
          }}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing || event.keyCode === 229) {
              if (event.key === "Enter") event.preventDefault();
              return;
            }
            if (event.key === "Escape" && !pending) {
              event.preventDefault();
              close();
            }
          }}
        >
          <Input
            controlSize="sm"
            aria-label={t("设备名称")}
            aria-invalid={!!error || undefined}
            aria-describedby={error ? errorId : undefined}
            autoFocus
            maxLength={80}
            value={draft}
            disabled={blocked}
            onFocus={(event) => event.currentTarget.select()}
            onChange={(event) => {
              setDraft(event.target.value);
              setError("");
            }}
          />
          {error && (
            <span className="device-name-error" role="alert" id={errorId} title={t(error)}>
              <WarningCircle size={16} aria-hidden="true" />
              <span className="device-name-error-tooltip">{t(error)}</span>
            </span>
          )}
          <IconButton
            controlSize="sm"
            type="submit"
            aria-label={t("保存设备名称")}
            title={t("保存设备名称")}
            disabled={blocked || !valid}
          >
            {pending ? <CircleNotch size={15} className="spin" /> : <Check size={15} />}
          </IconButton>
          <IconButton
            controlSize="sm"
            aria-label={t("取消编辑名称")}
            title={t("取消")}
            disabled={pending}
            onClick={close}
          >
            <X size={15} />
          </IconButton>
        </form>
      ) : (
        <IconButton
          ref={trigger}
          controlSize="sm"
          className="device-name-edit"
          aria-label={t("编辑设备名称")}
          title={t("编辑设备名称")}
          disabled={blocked}
          onClick={() => {
            setDraft(device.name);
            setError("");
            setEditing(true);
          }}
        >
          <PencilSimple size={15} aria-hidden="true" />
        </IconButton>
      )}
    </div>
  );
}
