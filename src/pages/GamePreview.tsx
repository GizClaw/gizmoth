import { t, useI18n } from "../i18n";
import { Select, IconButton, Button } from "../ui";
import { useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  ArrowRight,
  Check,
  ArrowBendUpLeft as CornerUpLeft,
  Cpu,
  Cube,
  Info,
  Keyboard,
  CircleNotch as LoaderCircle,
  Monitor,
  Power,
  ArrowsClockwise as RefreshCw,
  Square,
  X,
} from "@phosphor-icons/react";
import { createModelsProvider, loadModelPlay, type ModelFirmware } from "@gizclaw/models-player";
import { previewFirmwareProvider } from "../preview-firmware";
import { modelPreviewIssue } from "../preview-environment";
import type { ModelPlay, ModelPlayState } from "@gizclaw/models-player/play";
import type { H106Viewer } from "@gizclaw/models-player/h106";
import "@gizclaw/models-player/style.css";
import { errorMessage } from "../bridge";
import type { DeviceModel } from "../device-model";
import type { SimulatorProfile, SimulatorPatch } from "../simulator-profiles";
import type { SimulatorAppRequest } from "../simulator-window";
import { preparePreviewProject } from "../preview-project";
import { createPreviewInput, previewKey, type PreviewButton } from "../preview-input";

const models: DeviceModel[] = [
  { id: "h106-tiga", name: "H106 Tiga" },
  { id: "h106-zero", name: "H106 Zero" },
  { id: "h106-orb", name: "H106 Orb" },
  { id: "h106-zeta", name: "H106 Zeta" },
];
const initial: ModelPlayState = { mode: "preview", phase: "idle", firmware: null, fullscreen: "none", error: null };
const phases: Record<ModelPlayState["phase"], string> = {
  idle: "模型预览",
  installing: "正在载入固件",
  ready: "等待启动",
  starting: "正在启动",
  running: "试玩环境运行中",
  stopped: "已关机",
  error: "运行失败",
  destroyed: "已关闭",
};
const controls = [
  { button: "back", label: "返回", key: "Esc", Icon: CornerUpLeft },
  { button: "left", label: "左", key: "←", Icon: ArrowLeft },
  { button: "right", label: "右", key: "→", Icon: ArrowRight },
  { button: "ok", label: "确认", key: "Enter", Icon: Check },
  { button: "power", label: "电源", key: "长按 B", Icon: Power },
] as const;

export function GamePreview({
  deviceModel,
  incomingApp,
  preferences,
  onPreferences,
}: {
  preferences?: SimulatorProfile;
  onPreferences?: (patch: SimulatorPatch) => void;
  deviceModel?: DeviceModel;
  incomingApp?: SimulatorAppRequest | null;
}) {
  const { language } = useI18n();
  const mount = useRef<HTMLDivElement>(null);
  const viewerLanguage = useRef(language);
  viewerLanguage.current = language;
  const screen = useRef<HTMLCanvasElement>(null);
  const [view, setView] = useState<"model" | "screen">(preferences?.view ?? "model");
  const [firmwareOpen, setFirmwareOpen] = useState(false);
  const area = useRef<HTMLDivElement>(null);
  const player = useRef<ModelPlay | null>(null);
  const input = useRef<ReturnType<typeof createPreviewInput> | null>(null);
  const [model, setModel] = useState<DeviceModel["id"]>(deviceModel?.id ?? "h106-tiga");
  const [attempt, setAttempt] = useState(0);
  const [loaded, setLoaded] = useState(false);
  const [state, setState] = useState(initial);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [pressed, setPressed] = useState<PreviewButton[]>([]);
  const handledApp = useRef<string | null>(null);
  const latestApp = useRef(incomingApp);
  latestApp.current = incomingApp;
  const [notice, setNotice] = useState<{ message: string; name: string } | null>(null);
  const [dismissedApp, setDismissedApp] = useState<string | null>(null);
  const dismissedAppId = useRef<string | null>(null);
  const appNoticeVisible = incomingApp && incomingApp.id !== dismissedApp;
  const shownNotice = incomingApp && !appNoticeVisible ? "" : notice ? t(notice.message, { name: notice.name }) : "";
  const [firmwares, setFirmwares] = useState<readonly ModelFirmware[]>([]);
  const [firmwareId, setFirmwareId] = useState(preferences?.firmwareId ?? "");
  const [installation, setInstallation] = useState<{ id: string } | null>(null);
  const environmentIssue = modelPreviewIssue();
  const runtimeIssue =
    environmentIssue ??
    (!globalThis.crossOriginIsolated
      ? t("固件运行环境未完成隔离，请关闭并重新打开模拟器。")
      : typeof globalThis.SharedArrayBuffer !== "function"
        ? t("当前系统的 WebView 不支持固件线程，请更新系统后重试。")
        : null);
  const canPlay = !runtimeIssue;
  const running = state.phase === "running";
  const pending =
    !!busy ||
    state.phase === "installing" ||
    state.phase === "starting" ||
    (!loaded && !error && state.phase !== "error");
  const installed = firmwares.find((firmware) => firmware.id === state.firmware);
  const lock = useRef(false);

  useEffect(() => {
    let gone = false;
    let instance: ModelPlay | undefined;
    let unsubscribe = () => {};
    setLoaded(false);
    setState(initial);
    setError("");
    setNotice(null);
    handledApp.current = null;
    setPressed([]);
    if (environmentIssue) {
      setError(environmentIssue);
      setState({ ...initial, phase: "error", error: environmentIssue });
      setFirmwares([]);
      setFirmwareId("");
      return;
    }
    const host = document.createElement("div");
    host.className = "preview-player-root";
    mount.current?.append(host);
    void loadModelPlay()
      .then(({ createModelPlay }) => {
        if (gone) return;
        instance = createModelPlay(host, {
          model,
          language: viewerLanguage.current,
          embedded: true,
          presentation: "inline",
          shortcuts: false,
          modelsProvider: previewFirmwareProvider(createModelsProvider(), installation?.id ?? null, (choices) => {
            if (gone) return;
            setFirmwares(choices);
            setFirmwareId((selected) =>
              choices.some((firmware) => firmware.id === selected) ? selected : (choices[0]?.id ?? ""),
            );
          }),
          viewerOptions: { initialView: "screen", sound: false, simGuideButton: false },
          onStateChange: (next) => {
            if (!gone) setState(next);
          },
        });
        player.current = instance;
        return instance.ready.then(async () => {
          if (gone || !instance) return;
          const buttons = (instance.viewer as H106Viewer | null)?.buttons;
          if (buttons) {
            input.current = createPreviewInput(buttons);
            unsubscribe = buttons.subscribe(() => {
              if (!gone) setPressed(buttons.getPressed());
            });
          }
          setLoaded(true);
          if (installation) {
            setBusy("install");
            try {
              await instance.enter();
            } finally {
              if (!gone) setBusy("");
            }
          }
        });
      })
      .catch((e) => {
        if (!gone) setError(errorMessage(e));
      });
    return () => {
      gone = true;
      input.current?.releaseAll();
      input.current = null;
      unsubscribe();
      if (player.current === instance) player.current = null;
      instance?.destroy();
      host.remove();
    };
  }, [model, attempt, installation, environmentIssue]);

  useEffect(() => {
    const target = area.current;
    if (!target) return;
    const nativeActivation = (event: KeyboardEvent) =>
      !!(event.target as HTMLElement).closest("button") && (event.code === "Enter" || event.code === "Space");
    const editable = (event: KeyboardEvent) =>
      !!(event.target as HTMLElement).closest("input,textarea,select,[contenteditable=true]");
    const down = (event: KeyboardEvent) => {
      if (editable(event) || nativeActivation(event)) return;
      const button = previewKey(event);
      if (!button || !input.current) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      input.current.press(button, `preview:key:${event.code}`);
    };
    const up = (event: KeyboardEvent) => {
      if (!previewKey({ code: event.code })) return;
      input.current?.release(`preview:key:${event.code}`);
      if (!editable(event) && !nativeActivation(event)) {
        event.preventDefault();
        event.stopImmediatePropagation();
      }
    };
    const release = () => input.current?.releaseAll();
    const focusOut = (event: FocusEvent) => {
      if (!(event.relatedTarget instanceof Node) || !target.contains(event.relatedTarget)) release();
    };
    const hidden = () => {
      if (document.hidden) release();
    };
    target.addEventListener("keydown", down, true);
    target.addEventListener("keyup", up, true);
    target.addEventListener("focusout", focusOut, true);
    const focusIn = (event: FocusEvent) => {
      if (event.target instanceof Node && !target.contains(event.target)) release();
    };
    const releaseKey = (event: KeyboardEvent) => {
      input.current?.release(`preview:key:${event.code}`);
      for (const { button } of controls) input.current?.release(`preview:control:${button}:${event.code}`);
    };
    document.addEventListener("focusin", focusIn, true);
    window.addEventListener("keyup", releaseKey);
    window.addEventListener("blur", release);
    document.addEventListener("visibilitychange", hidden);
    return () => {
      release();
      target.removeEventListener("keydown", down, true);
      target.removeEventListener("keyup", up, true);
      target.removeEventListener("focusout", focusOut, true);
      document.removeEventListener("focusin", focusIn, true);
      window.removeEventListener("keyup", releaseKey);
      window.removeEventListener("blur", release);
      document.removeEventListener("visibilitychange", hidden);
    };
  }, []);

  useEffect(() => {
    const current = player.current;
    current?.setModelRenderingEnabled(view === "model");
    if (view !== "screen" || !current || !screen.current) return;
    const canvas = screen.current;
    const context = canvas.getContext("2d");
    if (!context) return;
    let frame = 0;
    const draw = () => {
      const display = current.display;
      context.globalAlpha = 1;
      context.fillStyle = "#000";
      context.fillRect(0, 0, canvas.width, canvas.height);
      if (display) {
        context.globalAlpha = display.displayBrightnessPercent / 100;
        context.imageSmoothingEnabled = false;
        context.drawImage(display.canvas, 0, 0, canvas.width, canvas.height);
      }
      frame = requestAnimationFrame(draw);
    };
    draw();
    return () => cancelAnimationFrame(frame);
  }, [view, loaded, model, attempt, installation]);

  async function task(label: string, action: (current: ModelPlay) => Promise<void>, relevant = () => true) {
    const current = player.current;
    if (!current || lock.current) return;
    lock.current = true;
    setBusy(label);
    setError("");
    setNotice(null);
    try {
      await action(current);
    } catch (e) {
      if (player.current === current && relevant()) {
        setNotice(null);
        setError(errorMessage(e));
      }
    } finally {
      lock.current = false;
      setBusy("");
    }
  }
  const installIncomingApp = (app: SimulatorAppRequest) =>
    task(
      "app",
      async (current) => {
        const pkg = await preparePreviewProject(app.snapshot);
        let packageConsumed = false;
        const isCurrent = () => player.current === current && latestApp.current?.id === app.id;
        try {
          if (!isCurrent()) {
            packageConsumed = true;
            return;
          }
          setNotice({ message: "正在安装「{name}」…", name: pkg.name });
          await current.executeCommand(
            { method: "tools.invoke", tool: "lua.app.install", args: { url: pkg.url, sha256: pkg.sha256 } },
            { timeout_ms: 60000 },
          );
          packageConsumed = true;
          if (!isCurrent()) return;
          setNotice({ message: "「{name}」已安装，正在请求运行…", name: pkg.name });
          await current.executeCommand({ method: "tools.invoke", tool: "lua.app.run", args: { app_id: pkg.appId } });
          if (isCurrent()) {
            setNotice({ message: "已请求运行「{name}」。长按返回可退出。", name: pkg.name });
            setView("screen");
          }
        } finally {
          // Timed-out installs may still be fetching. Unacknowledged packages expire on the server.
          if (packageConsumed) await pkg.dispose().catch(() => {});
        }
      },
      () => latestApp.current?.id === app.id && dismissedAppId.current !== app.id,
    );
  useEffect(() => {
    setNotice(null);
    setError("");
  }, [incomingApp?.id]);
  useEffect(() => {
    if (!incomingApp || !canPlay || !running || pending || lock.current || handledApp.current === incomingApp.id)
      return;
    handledApp.current = incomingApp.id;
    void installIncomingApp(incomingApp);
  }, [incomingApp, canPlay, running, pending]);
  const press = (button: PreviewButton, source: string) => input.current?.press(button, source);
  const release = (source: string) => input.current?.release(source);
  const rawError = environmentIssue || error || state.error;
  const shownError =
    rawError === "device operation failed"
      ? t("固件未接受请求，请检查应用是否与当前固件兼容。")
      : rawError
        ? t(rawError)
        : rawError;

  return (
    <aside className="game-preview" aria-label={t("模拟设备")}>
      <div className="preview-toolbar" aria-label={t("模拟器工具栏")}>
        <Select
          controlSize="sm"
          className="preview-model-select"
          aria-label={t("预览机型")}
          value={model}
          disabled={pending || running}
          onValueChange={(value) => {
            setFirmwareId("");
            setFirmwares([]);
            setInstallation(null);
            setView("model");
            setModel(value as DeviceModel["id"]);
            onPreferences?.({ model: value as DeviceModel["id"], firmwareId: "", view: "model" });
          }}
          options={models.map((item) => ({ value: item.id, label: item.name }))}
        />
        <div className="preview-view-switch" role="group" aria-label={t("预览视图")}>
          <Button
            controlSize="sm"
            variant="ghost"
            aria-label={t("设备模型")}
            title={t("设备模型")}
            aria-pressed={view === "model"}
            onClick={() => {
              setView("model");
              onPreferences?.({ view: "model" });
            }}
          >
            <Cube size={15} />
            <span>{t("模型")}</span>
          </Button>
          <Button
            controlSize="sm"
            variant="ghost"
            aria-label={t("游戏屏幕")}
            title={t("游戏屏幕")}
            aria-pressed={view === "screen"}
            onClick={() => {
              setView("screen");
              onPreferences?.({ view: "screen" });
            }}
          >
            <Monitor size={15} />
            <span>{t("屏幕")}</span>
          </Button>
        </div>
        <Button
          controlSize="sm"
          variant="ghost"
          className="preview-firmware-toggle"
          popoverTarget="preview-firmware-panel"
          aria-expanded={firmwareOpen}
          title={t("选择并安装固件")}
        >
          <Cpu size={15} />
          {t("固件")}
        </Button>
      </div>
      <div
        id="preview-firmware-panel"
        popover="auto"
        className="preview-popover preview-firmware-panel"
        aria-label={t("固件设置")}
        onToggle={(event) => setFirmwareOpen(event.currentTarget.matches(":popover-open"))}
      >
        <div className="preview-popover-heading">
          <h2>{t("固件")}</h2>
          <IconButton
            aria-label={t("关闭固件设置")}
            title={t("关闭")}
            popoverTarget="preview-firmware-panel"
            popoverTargetAction="hide"
          >
            <X size={14} />
          </IconButton>
        </div>
        <label className="preview-field">
          <span>{t("兼容固件")}</span>
          <Select
            aria-label={t("兼容固件")}
            value={firmwareId}
            disabled={!loaded || pending || running || !firmwares.length}
            onValueChange={(value) => {
              setFirmwareId(value);
              onPreferences?.({ firmwareId: value });
            }}
            options={
              firmwares.length
                ? firmwares.map((firmware) => ({ value: firmware.id, label: firmware.name }))
                : [
                    {
                      value: "",
                      label: shownError ? t("预览不可用") : loaded ? t("暂无兼容固件") : t("正在读取固件目录…"),
                    },
                  ]
            }
          />
        </label>
        <p className="preview-installed" title={installed?.name}>
          {state.phase === "installing"
            ? t("正在安装所选固件")
            : installed && state.phase !== "error"
              ? t(state.phase === "stopped" ? "已停止：{name}" : "当前：{name}", { name: installed.name })
              : t("尚未安装固件")}
        </p>
        {!canPlay && <p className="preview-environment-note">{runtimeIssue ? t(runtimeIssue) : null}</p>}
        <div className="preview-popover-actions">
          <Button
            controlSize="sm"
            variant="ghost"
            aria-label={t("重新加载预览")}
            disabled={pending}
            onClick={() => {
              if (!player.current) setAttempt((value) => value + 1);
              else
                void task("reload", async (current) => {
                  try {
                    await current.shutdown();
                  } finally {
                    setAttempt((value) => value + 1);
                  }
                });
            }}
          >
            <RefreshCw size={14} />
            {t("重新加载")}
          </Button>
          <Button
            controlSize="sm"
            variant="primary"
            disabled={!loaded || !canPlay || !firmwareId || pending || running}
            onClick={() =>
              void task("prepare-install", async (current) => {
                await current.shutdown();
                onPreferences?.({ firmwareId });
                setInstallation({ id: firmwareId });
              })
            }
          >
            {pending && installation ? <LoaderCircle size={13} className="spin" /> : <Cpu size={14} />}
            {pending && installation
              ? t("正在安装…")
              : state.firmware === firmwareId && installed
                ? t("重新安装")
                : t("安装固件")}
          </Button>
        </div>
      </div>
      <div
        id="preview-help-panel"
        popover="auto"
        className="preview-popover preview-help-panel"
        aria-label={t("运行与快捷键说明")}
      >
        <div className="preview-popover-heading">
          <h2>{t("键盘与设备按键")}</h2>
          <IconButton
            aria-label={t("关闭按键说明")}
            title={t("关闭")}
            popoverTarget="preview-help-panel"
            popoverTargetAction="hide"
          >
            <X size={14} />
          </IconButton>
        </div>
        <p id="preview-key-help">{t("点击设备区域后使用键盘。长按电源开机，长按返回退出应用。")}</p>
        <dl className="preview-shortcuts">
          {controls.map(({ button, label, key }) => (
            <div key={button}>
              <dt>{t(label)}</dt>
              <dd>
                <kbd>{t(key)}</kbd>
              </dd>
            </div>
          ))}
        </dl>
        {!canPlay && <p className="preview-environment-note">{runtimeIssue ? t(runtimeIssue) : null}</p>}
      </div>
      <div
        className="preview-play-area"
        ref={area}
        tabIndex={0}
        aria-label={t("游戏控制区域，左右方向键移动，Enter 确认，Esc 返回")}
        aria-describedby="preview-key-help"
      >
        <div className="preview-stage">
          <div
            className="preview-model-mount"
            role="group"
            aria-label={t(
              "设备模型。拖动可旋转；左右方向键控制游戏，Enter 确认，Esc 返回，B 电源。按键仅在预览区生效。",
            )}
            ref={mount}
            style={{ visibility: view === "model" && loaded ? "visible" : "hidden" }}
          />
          <canvas
            className="preview-screen"
            ref={screen}
            width={240}
            height={240}
            hidden={view !== "screen"}
            tabIndex={0}
            aria-label={t("游戏屏幕，左右键移动，Enter 确认，Esc 返回")}
          />
          {view === "screen" && loaded && !running && (
            <div className="preview-screen-placeholder" role="status">
              <Power size={22} aria-hidden="true" />
              <strong>{t("屏幕未启动")}</strong>
              <span>{canPlay ? t("安装固件后开机") : t("此版本暂不支持固件运行")}</span>
            </div>
          )}
          {!loaded && (
            <div className="preview-loading" role="status" aria-live="polite">
              {shownError ? (
                <Monitor size={22} aria-hidden="true" />
              ) : (
                <LoaderCircle size={20} className="spin" aria-hidden="true" />
              )}
              <span>{shownError ? t("预览暂时不可用") : t("正在加载设备…")}</span>
            </div>
          )}
        </div>
        <div className="preview-control-bar">
          <div className="preview-controls" role="group" aria-label={t("设备按键")}>
            {controls.map(({ button, label, key, Icon }) => (
              <Button
                key={button}
                controlSize="md"
                className={`game-key${button === "power" ? " game-power" : ""}${pressed.includes(button) ? " pressed" : ""}`}
                title={`${t(label)} · ${t(key)}`}
                aria-pressed={pressed.includes(button)}
                aria-label={t("{label}（{button}）", { label: t(label), button })}
                disabled={!loaded || !!busy}
                onPointerDown={(e) => {
                  if (e.button !== 0) return;
                  e.preventDefault();
                  e.currentTarget.focus();
                  e.currentTarget.setPointerCapture(e.pointerId);
                  press(button, `preview:pointer:${e.pointerId}`);
                }}
                onPointerUp={(e) => release(`preview:pointer:${e.pointerId}`)}
                onPointerCancel={(e) => release(`preview:pointer:${e.pointerId}`)}
                onLostPointerCapture={(e) => release(`preview:pointer:${e.pointerId}`)}
                onKeyDown={(e) => {
                  if (e.code === "Enter" || e.code === "Space") {
                    e.preventDefault();
                    e.stopPropagation();
                    press(button, `preview:control:${button}:${e.code}`);
                  }
                }}
                onKeyUp={(e) => {
                  if (e.code === "Enter" || e.code === "Space") {
                    e.preventDefault();
                    e.stopPropagation();
                    release(`preview:control:${button}:${e.code}`);
                  }
                }}
                onClick={(e) => {
                  if (e.detail === 0) {
                    const current = input.current;
                    current?.press(button, `preview:click:${button}`);
                    requestAnimationFrame(() => current?.release(`preview:click:${button}`));
                  }
                }}
              >
                <Icon size={18} />
                <span>{t(label)}</span>
              </Button>
            ))}
          </div>
          <IconButton
            controlSize="md"
            aria-label={t("键盘快捷键")}
            title={t("键盘快捷键")}
            popoverTarget="preview-help-panel"
          >
            <Keyboard size={17} />
          </IconButton>
        </div>
      </div>
      <div className="preview-runtime">
        <div className="preview-state" role="status">
          <i className={running ? "dot dot-ok" : "dot"} />
          {shownError && !loaded ? t("预览不可用") : loaded ? t(phases[state.phase]) : t("准备预览")}
        </div>
        {running ? (
          <Button
            controlSize="sm"
            disabled={pending}
            onClick={() => void task("stop", (current) => current.shutdown())}
          >
            <Square size={12} />
            {t("停止")}
          </Button>
        ) : state.phase === "ready" ? (
          <Button
            variant="primary"
            controlSize="sm"
            disabled={pending}
            onClick={() => void task("start", async (current) => current.start())}
          >
            <Power size={13} />
            {t("开机")}
          </Button>
        ) : !canPlay ? (
          <Button
            controlSize="sm"
            variant="ghost"
            className="preview-runtime-help"
            popoverTarget="preview-help-panel"
            title={t("查看运行支持说明")}
          >
            <Info size={14} />
            {t("仅预览")}
          </Button>
        ) : (
          <Button
            controlSize="sm"
            variant="ghost"
            className="preview-runtime-help"
            popoverTarget="preview-firmware-panel"
          >
            {environmentIssue ? t("预览不可用") : pending ? t("正在准备…") : t("尚未安装固件")}
          </Button>
        )}
      </div>
      {(appNoticeVisible || shownNotice || shownError) && (
        <div className="preview-feedback">
          {appNoticeVisible && (
            <div className="preview-incoming-app" aria-label={t("临时应用请求")}>
              <strong>{incomingApp.name}</strong>
              <p role="status">
                {!canPlay
                  ? runtimeIssue
                    ? t(runtimeIssue)
                    : null
                  : busy === "app"
                    ? t("正在处理应用安装请求…")
                    : handledApp.current === incomingApp.id
                      ? error
                        ? t("应用请求失败，可重试。")
                        : t("应用请求已处理。")
                      : t("应用待安装。安装并启动兼容固件后，将自动安装并请求运行。")}
              </p>
              <div>
                <Button
                  controlSize="sm"
                  onClick={() => {
                    // Dismiss presentation only; the active install/run request must stay current.
                    dismissedAppId.current = incomingApp.id;
                    setDismissedApp(incomingApp.id);
                    setNotice(null);
                    setError("");
                  }}
                >
                  {t("关闭提示")}
                </Button>
                {error && canPlay && (
                  <Button
                    controlSize="sm"
                    disabled={!running || pending}
                    onClick={() => void installIncomingApp(incomingApp)}
                  >
                    {t("重试安装")}
                  </Button>
                )}
              </div>
            </div>
          )}
          {(shownNotice || shownError) && (
            <div className="preview-notes">
              {shownNotice && <p role="status">{shownNotice}</p>}
              {shownError && (
                <p className="error" role="alert">
                  {shownError}
                </p>
              )}
            </div>
          )}
        </div>
      )}
    </aside>
  );
}
