import { useCallback, useEffect, useRef, useState } from "react";
import { Boxes, Code2, Cpu, Settings } from "lucide-react";
import { bridge, errorMessage, native, type Device, type ProjectSummary, type Provider } from "./bridge";
import type { Overview } from "./device-model";
import { pickedModels } from "./providers";
import { DeviceDetail, DevicesPage } from "./pages/Devices";
import { ProjectsPage } from "./pages/Projects";
import { ProvidersPage } from "./pages/Providers";
import { SettingsPage } from "./pages/Settings";
import { Workspace } from "./pages/Workspace";
import { Dialog, Dot, Mark } from "./ui";

type Route =
  | { page: "devices" }
  | { page: "device"; id: string }
  | { page: "projects" }
  | { page: "project"; id: string }
  | { page: "providers" }
  | { page: "settings" };

const mac = navigator.userAgent.includes("Mac");

export function App() {
  const [route, setRoute] = useState<Route>({ page: "projects" });
  const [devices, setDevices] = useState<Device[]>([]);
  const [overviews, setOverviews] = useState<Record<string, Overview>>({});
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [deviceId, setDeviceId] = useState(() => localStorage.getItem("gizmoth.device") ?? "");
  const [error, setError] = useState("");
  const [asking, setAsking] = useState<{ title: string; body: string; resolve: (ok: boolean) => void } | null>(null);
  const askingRef = useRef(asking);
  askingRef.current = asking;

  const loadDevices = useCallback(async () => {
    const list = await bridge.devices();
    setDevices(list);
    setDeviceId((current) => (list.some((d) => d.id === current) ? current : (list[0]?.id ?? "")));
  }, []);
  const loadProjects = useCallback(async () => setProjects(await bridge.projects()), []);
  const loadProviders = useCallback(async () => setProviders(await bridge.providers()), []);
  const projectChanged = useCallback(() => void loadProjects().catch((e) => setError(errorMessage(e))), [loadProjects]);

  useEffect(() => {
    if (!native) return;
    Promise.all([loadDevices(), loadProjects(), loadProviders()]).catch((e) => setError(errorMessage(e)));
  }, [loadDevices, loadProjects, loadProviders]);
  useEffect(() => localStorage.setItem("gizmoth.device", deviceId), [deviceId]);

  const confirm = useCallback(
    (title: string, body: string) =>
      new Promise<boolean>((resolve) => {
        askingRef.current?.resolve(false);
        setAsking({ title, body, resolve });
      }),
    [],
  );
  const answer = (ok: boolean) => {
    asking?.resolve(ok);
    setAsking(null);
  };

  const section = route.page === "device" ? "devices" : route.page === "project" ? "projects" : route.page;
  const nav = (
    page: "devices" | "projects" | "providers" | "settings",
    label: string,
    Icon: typeof Cpu,
    count?: number,
  ) => (
    <button className={section === page ? "nav-item active" : "nav-item"} onClick={() => setRoute({ page })}>
      <Icon size={15} strokeWidth={1.8} />
      <span>{label}</span>
      {count ? <span className="nav-count">{count}</span> : null}
    </button>
  );
  const online = devices.filter((d) => overviews[d.id]?.online).length;
  const modelCount = providers.reduce((n, p) => n + pickedModels(p).length, 0);
  const project = route.page === "project" ? projects.find((p) => p.id === route.id) : undefined;
  const device = route.page === "device" ? devices.find((d) => d.id === route.id) : undefined;

  return (
    <div className={mac ? "app mac" : "app"}>
      <aside className="sidebar">
        <div className="brand" data-tauri-drag-region>
          <Mark size={19} />
          <span>GizMoth</span>
        </div>
        <div className="nav-label">工作</div>
        {nav("projects", "项目", Code2, projects.length)}
        {nav("devices", "设备", Cpu, devices.length)}
        <div className="nav-label">模型</div>
        {nav("providers", "Providers", Boxes, providers.length)}
        <div className="sidebar-foot">
          {nav("settings", "设置", Settings)}
          <div className="gateway">
            <Dot tone={modelCount ? "ok" : "warn"} />
            <span>{modelCount ? `${modelCount} 个模型可用` : "未配置模型"}</span>
            {devices.length > 0 && (
              <span className="muted">
                · {online}/{devices.length} 在线
              </span>
            )}
          </div>
        </div>
      </aside>
      <main className="main">
        {error && <p className="error banner top">{error}</p>}
        {!native && (
          <p className="notice banner top">
            浏览器预览：界面可浏览，蓝牙、钥匙串、本地项目与模型请求需要在桌面应用中使用。
          </p>
        )}
        {route.page === "devices" && (
          <DevicesPage
            devices={devices}
            overviews={overviews}
            onOpen={(id) => setRoute({ page: "device", id })}
            onPaired={(d) => {
              void loadDevices();
              setDeviceId(d.id);
              setRoute({ page: "device", id: d.id });
            }}
          />
        )}
        {route.page === "device" && device && (
          <DeviceDetail
            device={device}
            overview={overviews[device.id]}
            onOverview={(o) =>
              setOverviews((all) => {
                const next = { ...all };
                if (o) next[device.id] = o;
                else delete next[device.id];
                return next;
              })
            }
            onBack={() => setRoute({ page: "devices" })}
            onRemoved={() => {
              void loadDevices();
              setRoute({ page: "devices" });
            }}
          />
        )}
        {route.page === "projects" && (
          <ProjectsPage
            projects={projects}
            onOpen={(id) => setRoute({ page: "project", id })}
            onChanged={loadProjects}
            confirm={confirm}
            onCreated={(p) => {
              void loadProjects();
              setProjects((all) => [p, ...all.filter((item) => item.id !== p.id)]);
              setRoute({ page: "project", id: p.id });
            }}
          />
        )}
        {route.page === "project" && project && (
          <Workspace
            key={project.id}
            project={project}
            devices={devices}
            deviceId={deviceId}
            onDevice={setDeviceId}
            providers={providers}
            confirm={confirm}
            onBack={() => setRoute({ page: "projects" })}
            onChanged={projectChanged}
            onDeleted={() => {
              void loadProjects();
              setRoute({ page: "projects" });
            }}
            onOpenProviders={() => setRoute({ page: "providers" })}
          />
        )}
        {route.page === "providers" && (
          <ProvidersPage
            providers={providers}
            devices={devices}
            onChanged={loadProviders}
            onOpenDevices={() => setRoute({ page: "devices" })}
          />
        )}
        {route.page === "settings" && <SettingsPage />}
      </main>
      {asking && (
        <Dialog title={asking.title} onClose={() => answer(false)}>
          <p className="dialog-text">{asking.body}</p>
          <div className="dialog-actions">
            <button className="button" onClick={() => answer(false)}>
              拒绝
            </button>
            <button className="button primary" onClick={() => answer(true)}>
              允许
            </button>
          </div>
        </Dialog>
      )}
    </div>
  );
}
