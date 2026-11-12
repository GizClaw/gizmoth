import { IconButton, Button } from "./ui";
import { lazy, Suspense, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  BookOpen,
  CaretDown,
  Circuitry as CircuitBoard,
  Code as Code2,
  Cpu,
  SidebarSimple as PanelLeftClose,
  Sidebar as PanelLeftOpen,
  Monitor,
  ArrowsClockwise as RefreshCw,
  PlugsConnected,
  Robot,
  Toolbox,
  Wrench,
} from "@phosphor-icons/react";
import { AboutPage } from "./pages/About";
import { PageCompanion } from "./PageCompanion";
import { useI18n } from "./i18n";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openSimulator } from "./simulator-window";
import { useSimulatorCount } from "./simulator-profiles";
import { bridge, demo, errorMessage, native, type Device, type ProjectSummary, type Provider } from "./bridge";
import type { ConfirmLabels } from "./agent";
import { deviceModelOf, parseOverview, type Overview } from "./device-model";
import { DeviceDetail, DevicesPage } from "./pages/Devices";
import { ProjectsPage } from "./pages/Projects";
import { ProvidersPage } from "./pages/Providers";
import { SettingsPage } from "./pages/Settings";
import { SettingsNavigation, type SettingsSection } from "./pages/SettingsNavigation";
import { OpenSourcePage } from "./pages/OpenSource";
import { Workspace } from "./pages/Workspace";
import { Dialog, Dot, Mark, TitlebarContext } from "./ui";
import { UpdateButton } from "./UpdateButton";
import { projectKindOf, projectKindOrder, projectKinds, type ProjectKind } from "./project-kinds";

import { SimulatorManager } from "./pages/SimulatorManager";
import { SerialPage, SerialPortPage } from "./pages/Serial";
import { ServicePage } from "./pages/Service";
import { useSerialPortCount } from "./serial";

const projectKindIcons = { lua: Code2, giz: Cpu, rom: CircuitBoard };
const DocumentationPage = lazy(() => import("./pages/Documentation"));
const AgentsPage = lazy(() => import("./pages/Agents").then(({ AgentsPage }) => ({ default: AgentsPage })));
const ChipsPage = lazy(() => import("./pages/DevEnv").then(({ ChipsPage }) => ({ default: ChipsPage })));
const EnvironmentsPage = lazy(() =>
  import("./pages/DevEnv").then(({ EnvironmentsPage }) => ({ default: EnvironmentsPage })),
);
const ToolchainsPage = lazy(() => import("./pages/DevEnv").then(({ ToolchainsPage }) => ({ default: ToolchainsPage })));

type Route =
  | { page: "devices" }
  | { page: "simulators" }
  | { page: "serial" }
  | { page: "serial-port"; id: string }
  | { page: "device"; id: string }
  | { page: "projects"; kind: ProjectKind }
  | { page: "project"; id: string }
  | { page: "docs" }
  | { page: "agents" }
  | { page: "chips" }
  | { page: "environments" }
  | { page: "toolchains" }
  | { page: "settings"; section?: SettingsSection };

// Desktop availability is explained once, above the active page's content.
const desktopNotices: Partial<Record<Route["page"] | SettingsSection, string>> = {
  projects: "请在桌面应用中管理本地项目。",
  project: "请在桌面应用中管理本地项目。",
  devices: "请在桌面应用中管理 GizClaw 设备。",
  device: "请在桌面应用中管理 GizClaw 设备。",
  serial: "请在桌面应用中管理串口。",
  "serial-port": "请在桌面应用中管理串口。",
  agents: "请在桌面应用中管理 agent Skill。",
  general: "请在桌面应用中管理后台运行与联网搜索设置。",
  providers: "请在桌面应用中管理 Provider 和模型。",
  service: "请在桌面应用中管理 GizMoth 服务。",
  about: "请在桌面应用中检查和安装更新。",
};

type TrayAction =
  { kind: "navigate"; page: "devices" | "simulators" | "settings" } | { kind: "open-simulator"; profileId: string };

const mac = isTauri() && navigator.userAgent.includes("Mac");

export function App() {
  const { t } = useI18n();
  const serialPorts = useSerialPortCount();
  const simulatorCount = useSimulatorCount();
  const [route, setRoute] = useState<Route>(() =>
    demo && new URLSearchParams(location.search).get("view") === "device"
      ? { page: "device", id: "demo-1" }
      : { page: "projects", kind: projectKindOrder[0] },
  );
  const [devices, setDevices] = useState<Device[]>([]);
  const [overviews, setOverviews] = useState<Record<string, Overview>>({});
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [deviceId, setDeviceId] = useState(() => localStorage.getItem("gizmoth.device") ?? "");
  const [collapsed, setCollapsed] = useState(() => localStorage.getItem("gizmoth.sidebar") === "collapsed");
  const [collapsedGroups, setCollapsedGroups] = useState(() => ({
    projects: localStorage.getItem("gizmoth.sidebar.projects") === "collapsed",
    devices: localStorage.getItem("gizmoth.sidebar.devices") === "collapsed",
    devenv: localStorage.getItem("gizmoth.sidebar.devenv") === "collapsed",
  }));
  const [fullscreen, setFullscreen] = useState(false);
  useEffect(() => {
    if (!mac) return;
    const appWindow = getCurrentWindow();
    let disposed = false;
    const syncFullscreen = async () => {
      const fullscreen = await appWindow.isFullscreen();
      if (!disposed) setFullscreen(fullscreen);
    };
    const listening = appWindow.onResized(() => void syncFullscreen().catch(console.error));
    // Subscribe before reading so a fullscreen transition during startup is not missed.
    void listening.then(syncFullscreen).catch(console.error);
    return () => {
      disposed = true;
      void listening.then((unlisten) => unlisten()).catch(console.error);
    };
  }, []);
  const [settingsExpanded, setSettingsExpanded] = useState(false);
  const [titlebar, setTitlebar] = useState<HTMLDivElement | null>(null);
  useEffect(() => {
    if (route.page === "settings") setSettingsExpanded(true);
  }, [route.page]);
  const prepareProjectLeave = useRef<(() => Promise<void>) | null>(null);
  const navigationRequest = useRef(0);
  const registerProjectLeave = useCallback((prepare: () => Promise<void>) => {
    prepareProjectLeave.current = prepare;
    return () => {
      if (prepareProjectLeave.current === prepare) prepareProjectLeave.current = null;
    };
  }, []);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState("");
  const [asking, setAsking] = useState<{
    title: string;
    body: string;
    labels?: ConfirmLabels;
    resolve: (ok: boolean) => void;
  } | null>(null);
  const askingRef = useRef(asking);
  askingRef.current = asking;
  const deviceRequest = useRef(0);

  const loadDevices = useCallback(async () => {
    const request = ++deviceRequest.current;
    const list = await bridge.devices();
    if (request !== deviceRequest.current) return;
    setDevices(list);
    setDeviceId((current) => (list.some((d) => d.id === current) ? current : (list[0]?.id ?? "")));
    const states = await Promise.allSettled(
      list.map(async (d) => [d.id, parseOverview(await bridge.overview(d.id))] as const),
    );
    if (request !== deviceRequest.current) return;
    setOverviews(Object.fromEntries(states.flatMap((result) => (result.status === "fulfilled" ? [result.value] : []))));
  }, []);
  const loadProjects = useCallback(async () => setProjects(await bridge.projects()), []);
  const loadProviders = useCallback(async () => setProviders(await bridge.providers()), []);
  const projectChanged = useCallback(() => void loadProjects().catch((e) => setError(errorMessage(e))), [loadProjects]);

  useEffect(() => {
    if (!native) return;
    Promise.all([loadDevices(), loadProjects(), loadProviders()]).catch((e) => setError(errorMessage(e)));
  }, [loadDevices, loadProjects, loadProviders]);
  useEffect(() => localStorage.setItem("gizmoth.device", deviceId), [deviceId]);
  useEffect(() => localStorage.setItem("gizmoth.sidebar", collapsed ? "collapsed" : "expanded"), [collapsed]);
  useEffect(() => {
    for (const [id, collapsed] of Object.entries(collapsedGroups))
      localStorage.setItem(`gizmoth.sidebar.${id}`, collapsed ? "collapsed" : "expanded");
  }, [collapsedGroups]);

  const confirm = useCallback(
    (title: string, body: string, labels?: ConfirmLabels) =>
      new Promise<boolean>((resolve) => {
        askingRef.current?.resolve(false);
        setAsking({ title, body, labels, resolve });
      }),
    [],
  );
  const answer = (ok: boolean) => {
    asking?.resolve(ok);
    setAsking(null);
  };
  const navigate = useCallback(async (next: Route, options?: { keepSidebarOpen?: boolean }) => {
    const request = ++navigationRequest.current;
    try {
      await prepareProjectLeave.current?.();
      if (request !== navigationRequest.current) return;
      setError("");
      setRoute(next);
      if (!options?.keepSidebarOpen && window.matchMedia("(max-width: 760px)").matches) setCollapsed(true);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, []);
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    const consume = async () => {
      try {
        const action = await invoke<TrayAction | null>("desktop_take_tray_action");
        if (disposed || !action) return;
        if (action.kind === "navigate") await navigate({ page: action.page });
        else await openSimulator({ profileId: action.profileId });
      } catch (error) {
        if (disposed) return;
        setError(errorMessage(error));
        const window = getCurrentWindow();
        void window
          .show()
          .then(() => window.setFocus())
          .catch(() => {});
      }
    };
    const listening = listen("desktop-tray-action", () => void consume());
    // Drain after listening so tray clicks during startup are not lost.
    void listening
      .then(() => {
        if (!disposed) void consume();
      })
      .catch((error) => {
        if (!disposed) setError(errorMessage(error));
      });
    return () => {
      disposed = true;
      void listening.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [navigate]);
  useEffect(() => {
    if (!isTauri()) return;
    const watching = ["simulator-delete-failed", "simulator-close-failed"].map((name) =>
      listen<string>(name, (event) => setError(event.payload)),
    );
    return () => {
      for (const pending of watching) void pending.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);
  useEffect(() => {
    const open = () => void navigate({ page: "simulators" });
    window.addEventListener("open-simulator-manager", open);
    return () => window.removeEventListener("open-simulator-manager", open);
  }, [navigate]);
  const paired = (device: Device) => {
    void loadDevices().catch((e) => setError(errorMessage(e)));
    setDeviceId(device.id);
    void navigate({ page: "device", id: device.id });
  };
  async function refresh() {
    setRefreshing(true);
    setError("");
    try {
      await Promise.all([loadDevices(), loadProjects(), loadProviders()]);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setRefreshing(false);
    }
  }

  const openProject = (id: string) => {
    setCollapsed(window.matchMedia("(max-width: 760px)").matches);
    void navigate({ page: "project", id });
  };
  const section =
    route.page === "device"
      ? "devices"
      : route.page === "project"
        ? "projects"
        : route.page === "serial-port"
          ? "serial"
          : route.page;
  const nav = (page: "docs" | "agents", label: string, Icon: typeof Cpu, count?: number) => (
    <button
      data-page={page}
      className={section === page ? "nav-item active" : "nav-item"}
      onClick={() => void navigate({ page })}
      title={collapsed ? t(label) : undefined}
      aria-label={t(label)}
      aria-current={route.page === page ? "page" : undefined}
    >
      <Icon size={16} />
      <span className="nav-text">{t(label)}</span>
      {count ? <span className="nav-count">{count}</span> : null}
    </button>
  );
  const toggleSettings = () => {
    const expand = collapsed || !settingsExpanded || route.page !== "settings";
    setSettingsExpanded(expand);
    if (expand) {
      setCollapsed(false);
      if (route.page !== "settings") void navigate({ page: "settings" }, { keepSidebarOpen: true });
    }
  };
  const deviceGroup: string[] = ["devices", "serial", "simulators"];
  const navGroup = (
    id: keyof typeof collapsedGroups,
    label: string,
    Icon: typeof Cpu,
    active: boolean,
    onCompact: () => void,
    children: ReactNode,
  ) => (
    <>
      <button
        type="button"
        className={`nav-item nav-heading${active && collapsedGroups[id] ? " active" : ""}`}
        aria-label={t(label)}
        aria-expanded={!collapsedGroups[id]}
        aria-controls={collapsedGroups[id] ? undefined : `nav-${id}`}
        onClick={() => setCollapsedGroups((groups) => ({ ...groups, [id]: !groups[id] }))}
      >
        <Icon size={16} />
        <span className="nav-text">{t(label)}</span>
        <CaretDown size={12} className="nav-group-caret" aria-hidden="true" />
      </button>
      <button
        type="button"
        data-group={id}
        className={`nav-item nav-group-compact${active ? " active" : ""}`}
        title={t(label)}
        aria-label={t(label)}
        onClick={onCompact}
      >
        <Icon size={16} />
      </button>
      {!collapsedGroups[id] && (
        <nav id={`nav-${id}`} className="nav-children" aria-label={t(label)}>
          {children}
        </nav>
      )}
    </>
  );
  const navChild = ({
    key,
    label,
    Icon,
    active,
    count,
    attributes,
    onClick,
  }: {
    key: string;
    label: string;
    Icon: typeof Cpu;
    active: boolean;
    count?: number;
    attributes?: Record<string, string>;
    onClick: () => void;
  }) => (
    <button
      key={key}
      type="button"
      className={active ? "nav-child active" : "nav-child"}
      aria-current={active ? "page" : undefined}
      {...attributes}
      onClick={onClick}
    >
      <Icon size={14} />
      <span>{label}</span>
      {count ? <span className="nav-count">{count}</span> : null}
    </button>
  );
  const devenvGroup: string[] = ["chips", "environments", "toolchains"];
  const inGroup = section === "projects" || deviceGroup.includes(section) || devenvGroup.includes(section);
  const hasNestedNavigation = inGroup || section === "settings" || settingsExpanded;
  const online = devices.filter((d) => overviews[d.id]?.online).length;
  const modelCount = providers.reduce((n, p) => n + p.models.length, 0);
  const project = route.page === "project" ? projects.find((p) => p.id === route.id) : undefined;
  const selectedKind = project ? projectKindOf(project) : route.page === "projects" ? route.kind : undefined;
  const desktopNotice = desktopNotices[route.page === "settings" ? (route.section ?? "general") : route.page];
  const device = route.page === "device" ? devices.find((d) => d.id === route.id) : undefined;
  const counts = projects.reduce<Partial<Record<ProjectKind, number>>>((all, p) => {
    const kind = projectKindOf(p);
    all[kind] = (all[kind] ?? 0) + 1;
    return all;
  }, {});

  return (
    <TitlebarContext.Provider value={titlebar}>
      <div
        className={`app${mac && !fullscreen ? " mac" : ""}${collapsed ? " sidebar-collapsed" : ""}${project ? " has-project" : ""}${inGroup ? " has-project-navigation" : ""}${hasNestedNavigation ? " has-nested-navigation" : ""}`}
      >
        <header className="app-titlebar" data-tauri-drag-region data-peetty="avoid">
          <div className="titlebar-brand" data-tauri-drag-region>
            <IconButton
              className="sidebar-toggle"
              onClick={() => setCollapsed((value) => !value)}
              aria-label={t(collapsed ? "展开侧边栏" : "收起侧边栏")}
              aria-expanded={!collapsed}
              title={t(collapsed ? "展开侧边栏" : "收起侧边栏")}
            >
              {collapsed ? <PanelLeftOpen size={16} /> : <PanelLeftClose size={16} />}
            </IconButton>
            <Mark size={20} />
            <span data-tauri-drag-region>GizMoth</span>
          </div>
          <div className="titlebar-page" ref={setTitlebar} />
          <div className="titlebar-utilities">
            {demo && (
              <span className="demo-indicator" title={t("演示模式：设备操作仅模拟；模型试玩在本机运行。")}>
                {t("演示")}
              </span>
            )}
            {/* Development-environment pages carry their own registry refresh. */}
            {route.page !== "device" && !devenvGroup.includes(route.page) && (
              <IconButton
                aria-label={t("刷新设备与项目")}
                title={t("刷新设备与项目")}
                disabled={!native || refreshing}
                onClick={() => void refresh()}
              >
                <RefreshCw size={15} className={refreshing ? "spin" : undefined} />
              </IconButton>
            )}
            <UpdateButton />
          </div>
        </header>
        <aside className="sidebar" aria-label={t("主导航")}>
          <div className="nav-label">{t("工作")}</div>
          {/* Group headings toggle visibility; only their children navigate. */}
          {navGroup(
            "projects",
            "项目",
            Code2,
            section === "projects",
            () => void navigate({ page: "projects", kind: selectedKind ?? projectKindOrder[0] }),
            projectKindOrder.map((kind, index) =>
              navChild({
                key: kind,
                label: t(projectKinds[kind].label),
                Icon: projectKindIcons[kind],
                active: selectedKind === kind,
                count: counts[kind],
                // The first kind stands in for "open Projects" in UI tests.
                attributes: { "data-project-kind": kind, ...(index === 0 ? { "data-page": "projects" } : {}) },
                onClick: () => void navigate({ page: "projects", kind }),
              }),
            ),
          )}
          {navGroup(
            "devices",
            "设备",
            Cpu,
            deviceGroup.includes(section),
            () => void navigate({ page: deviceGroup.includes(section) ? section : "devices" } as Route),
            [
              navChild({
                key: "devices",
                label: "GizClaw",
                Icon: Cpu,
                active: section === "devices",
                count: devices.length,
                attributes: { "data-page": "devices" },
                onClick: () => void navigate({ page: "devices" }),
              }),
              navChild({
                key: "serial",
                label: t("串口"),
                Icon: PlugsConnected,
                active: section === "serial",
                count: serialPorts,
                attributes: { "data-page": "serial" },
                onClick: () => void navigate({ page: "serial" }),
              }),
              navChild({
                key: "simulators",
                label: t("模拟器"),
                Icon: Monitor,
                active: section === "simulators",
                count: simulatorCount,
                attributes: { "data-page": "simulators" },
                onClick: () => void navigate({ page: "simulators" }),
              }),
            ],
          )}
          {navGroup(
            "devenv",
            "开发环境",
            Toolbox,
            devenvGroup.includes(section),
            () => void navigate({ page: devenvGroup.includes(section) ? section : "chips" } as Route),
            [
              navChild({
                key: "chips",
                label: t("芯片"),
                Icon: Cpu,
                active: section === "chips",
                attributes: { "data-page": "chips" },
                onClick: () => void navigate({ page: "chips" }),
              }),
              navChild({
                key: "environments",
                label: t("通用环境"),
                Icon: Toolbox,
                active: section === "environments",
                attributes: { "data-page": "environments" },
                onClick: () => void navigate({ page: "environments" }),
              }),
              navChild({
                key: "toolchains",
                label: t("工具链"),
                Icon: Wrench,
                active: section === "toolchains",
                attributes: { "data-page": "toolchains" },
                onClick: () => void navigate({ page: "toolchains" }),
              }),
            ],
          )}
          {nav("agents", "Agents", Robot)}
          {nav("docs", "文档", BookOpen)}
          <div className="sidebar-foot">
            <SettingsNavigation
              expanded={settingsExpanded && !collapsed}
              active={route.page === "settings" ? (route.section ?? "general") : undefined}
              onToggle={toggleSettings}
              onSelect={(section) => void navigate({ page: "settings", section })}
            />
            <div
              className="gateway"
              title={t("{online}/{total} 台设备在线 · {models} 个模型可用", {
                online,
                total: devices.length,
                models: modelCount,
              })}
            >
              <Dot tone={online ? "ok" : "idle"} />
              <span>
                {devices.length
                  ? t("{online}/{total} 台设备在线", { online, total: devices.length })
                  : t("尚未连接设备")}
              </span>
            </div>
          </div>
        </aside>
        <main className="main">
          {!native && desktopNotice && (
            <p className="notice banner top desktop-notice" role="note">
              {t(desktopNotice)}
            </p>
          )}
          {error && (
            <p className="error banner top" role="alert">
              {error}
            </p>
          )}
          {route.page === "devices" && (
            <DevicesPage
              devices={devices}
              overviews={overviews}
              onOpen={(id) => setRoute({ page: "device", id })}
              onPaired={paired}
            />
          )}
          {route.page === "device" && device && (
            <DeviceDetail
              key={device.id}
              device={device}
              overview={overviews[device.id]}
              onRenamed={(updated) => setDevices((all) => all.map((item) => (item.id === updated.id ? updated : item)))}
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
                void loadDevices().catch((e) => setError(errorMessage(e)));
                setRoute({ page: "devices" });
              }}
            />
          )}
          {route.page === "projects" && (
            <ProjectsPage
              projects={projects}
              kind={route.kind}
              onKind={(kind) => void navigate({ page: "projects", kind })}
              onOpen={openProject}
              onCreated={(p) => {
                void loadProjects().catch((e) => setError(errorMessage(e)));
                setProjects((all) => [p, ...all]);
                openProject(p.id);
              }}
            />
          )}
          {route.page === "project" && project && (
            <Workspace
              key={project.id}
              project={project}
              onBeforeLeave={registerProjectLeave}
              devices={devices}
              deviceId={deviceId}
              onDevice={setDeviceId}
              previewModel={deviceModelOf(
                overviews[deviceId]?.firmwareVersion,
                devices.find((d) => d.id === deviceId)?.icon,
              )}
              providers={providers}
              confirm={confirm}
              onBack={() => void navigate({ page: "projects", kind: projectKindOf(project) })}
              onChanged={projectChanged}
              onDeleted={() => {
                void loadProjects().catch((e) => setError(errorMessage(e)));
                setRoute({ page: "projects", kind: projectKindOf(project) });
              }}
              onOpenProviders={() => void navigate({ page: "settings", section: "providers" })}
            />
          )}
          {route.page === "simulators" && <SimulatorManager />}
          {route.page === "serial" && (
            <SerialPage
              onOpen={(id) => setRoute({ page: "serial-port", id })}
              onService={() => void navigate({ page: "settings", section: "service" })}
            />
          )}
          {route.page === "serial-port" && <SerialPortPage id={route.id} onBack={() => setRoute({ page: "serial" })} />}
          {route.page === "agents" && (
            <Suspense
              fallback={
                <p className="notice banner" role="status">
                  {t("正在读取接入状态…")}
                </p>
              }
            >
              <AgentsPage
                onSerial={() => void navigate({ page: "serial" })}
                onService={() => void navigate({ page: "settings", section: "service" })}
              />
            </Suspense>
          )}
          {(route.page === "chips" || route.page === "environments" || route.page === "toolchains") && (
            <Suspense
              fallback={
                <p className="notice banner" role="status">
                  {t("正在读取开发环境…")}
                </p>
              }
            >
              {route.page === "chips" ? (
                <ChipsPage onToolchains={() => void navigate({ page: "toolchains" })} />
              ) : route.page === "environments" ? (
                <EnvironmentsPage onToolchains={() => void navigate({ page: "toolchains" })} />
              ) : (
                <ToolchainsPage />
              )}
            </Suspense>
          )}
          {route.page === "docs" && (
            <Suspense
              fallback={
                <p className="notice banner" role="status">
                  {t("正在打开本机文档…")}
                </p>
              }
            >
              <DocumentationPage />
            </Suspense>
          )}
          {route.page === "settings" &&
            (route.section === "open-source" ? (
              <OpenSourcePage onGeneral={() => void navigate({ page: "settings", section: "general" })} />
            ) : route.section === "providers" ? (
              <ProvidersPage providers={providers} onChanged={loadProviders} />
            ) : route.section === "service" ? (
              <ServicePage />
            ) : route.section === "about" ? (
              <AboutPage />
            ) : (
              <SettingsPage />
            ))}
        </main>
        {asking && (
          <Dialog title={t(asking.title)} onClose={() => answer(false)}>
            <p className="dialog-text">{t(asking.body)}</p>
            <div className="dialog-actions">
              <Button onClick={() => answer(false)}>{t(asking.labels?.cancel ?? "拒绝")}</Button>
              <Button variant={asking.labels?.danger ? "danger" : "primary"} onClick={() => answer(true)}>
                {t(asking.labels?.confirm ?? "允许")}
              </Button>
            </div>
          </Dialog>
        )}
      </div>
      <PageCompanion />
    </TitlebarContext.Provider>
  );
}
