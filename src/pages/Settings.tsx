import { useEffect, useState } from "react";
import { bridge, errorMessage, native } from "../bridge";
import { Mark, PageHeader } from "../ui";

export function SettingsPage() {
  const [keepInTray, setKeepInTray] = useState<boolean | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    if (native)
      void bridge
        .preferences()
        .then((p) => setKeepInTray(p.keepInTray))
        .catch((e) => setError(errorMessage(e)));
  }, []);
  return (
    <>
      <PageHeader crumbs={[{ label: "设置" }]} />
      <div className="page narrow">
        {error && <p className="error banner">{error}</p>}
        <section className="card list">
          <div className="list-head">
            <span>窗口</span>
          </div>
          <div className="row">
            <span className="row-main">
              <span className="row-title">关闭窗口后继续在菜单栏运行</span>
              <span className="row-meta">从菜单栏图标重新打开；选择「退出 Gizmoth」完全退出。</span>
            </span>
            <input
              type="checkbox"
              className="switch"
              aria-label="关闭窗口后继续运行"
              checked={keepInTray ?? false}
              disabled={keepInTray === null}
              onChange={(e) => {
                const value = e.target.checked;
                bridge
                  .setPreferences(value)
                  .then(() => setKeepInTray(value))
                  .catch((err) => setError(errorMessage(err)));
              }}
            />
          </div>
        </section>
        <section className="card list">
          <div className="list-head">
            <span>本机数据</span>
          </div>
          <div className="row">
            <span className="row-main">
              <span className="row-title">设备凭据与 API key</span>
              <span className="row-meta">保存在系统钥匙串（macOS Keychain / Windows 凭据管理器）。</span>
            </span>
          </div>
          <div className="row">
            <span className="row-main">
              <span className="row-title">项目、对话记录与配置</span>
              <span className="row-meta">
                保存在应用数据目录：设备列表、Provider 配置、Lua 项目目录和每个项目的助手对话。
              </span>
            </span>
          </div>
        </section>
        <section className="card about">
          <span className="about-mark">
            <Mark size={30} />
          </span>
          <div>
            <h2>Gizmoth 0.1.0</h2>
            <p className="muted">
              配对、控制 GizOS 设备，并用编程助手开发 Lua 程序。MIT License · github.com/GizClaw/gizmoth
            </p>
          </div>
        </section>
      </div>
    </>
  );
}
