import { t, useI18n } from "../i18n";
import { useEffect, useState } from "react";
import { CaretRight, Cpu, Monitor, Plus } from "@phosphor-icons/react";
import { errorMessage, type Device } from "../bridge";
import { Button, Dialog } from "../ui";
import {
  simulatorModels,
  simulatorProfiles,
  watchSimulatorProfiles,
  showSimulatorManager,
  type SimulatorProfile,
} from "../simulator-profiles";

export function RunTargetDialog({
  devices,
  onDevice,
  onSimulator,
  onClose,
}: {
  devices: Device[];
  onDevice: (device: Device) => void;
  onSimulator: (id: string) => void;
  onClose: () => void;
}) {
  useI18n();
  const [profiles, setProfiles] = useState<SimulatorProfile[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  useEffect(() => {
    let gone = false;
    const refresh = () =>
      void simulatorProfiles
        .list()
        .then((all) => {
          if (!gone) {
            setProfiles(all);
            setError("");
          }
        })
        .catch((e) => {
          if (!gone) setError(errorMessage(e));
        })
        .finally(() => {
          if (!gone) setLoading(false);
        });
    refresh();
    const unwatch = watchSimulatorProfiles(refresh);
    return () => {
      gone = true;
      unwatch();
    };
  }, []);
  return (
    <Dialog title={t("选择运行目标")} onClose={onClose} className="run-target-dialog">
      <p className="dialog-text">{t("选择要运行此应用的设备。")}</p>
      <section className="run-target-group" aria-labelledby="run-hardware-heading">
        <h3 id="run-hardware-heading">{t("硬件")}</h3>
        {devices.length ? (
          <ul className="run-target-list">
            {devices.map((device) => (
              <li key={device.id}>
                <button type="button" className="run-target-row" onClick={() => onDevice(device)}>
                  <Cpu size={18} aria-hidden="true" />
                  <strong>{device.name}</strong>
                  <CaretRight size={14} aria-hidden="true" />
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p className="hint">{t("尚未连接硬件，可从左侧连接设备。")}</p>
        )}
      </section>
      <section className="run-target-group" aria-labelledby="run-simulator-heading">
        <h3 id="run-simulator-heading">{t("模拟器")}</h3>
        {loading ? (
          <p className="hint" role="status">
            {t("正在读取模拟器…")}
          </p>
        ) : profiles.length ? (
          <ul className="run-target-list">
            {profiles.map((profile) => (
              <li key={profile.id}>
                <button type="button" className="run-target-row" onClick={() => onSimulator(profile.id)}>
                  <Monitor size={18} aria-hidden="true" />
                  <strong>{profile.name}</strong>
                  <span>{simulatorModels.find((model) => model.id === profile.model)?.name}</span>
                  <CaretRight size={14} aria-hidden="true" />
                </button>
              </li>
            ))}
          </ul>
        ) : (
          !error && (
            <Button
              controlSize="sm"
              onClick={() => {
                onClose();
                showSimulatorManager();
              }}
            >
              <Plus size={14} />
              {t("创建模拟器")}
            </Button>
          )
        )}
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </section>
    </Dialog>
  );
}
