import { t, useI18n } from "../i18n";
import { IconButton, Button, relativeTime } from "../ui";
import { AgentModelPicker } from "./AgentModelPicker";
import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
} from "react";
import { createPortal } from "react-dom";
import type { AgentMessage } from "@mariozechner/pi-agent-core";
import {
  ArrowElbowDownLeft,
  Brain,
  CaretRight,
  Check,
  CircleNotch as LoaderCircle,
  Clock,
  MinusCircle,
  Paperclip,
  ArrowClockwise,
  ArrowCounterClockwise,
  Broom,
  Copy,
  X,
  Sparkle as Sparkles,
  Warning as TriangleAlert,
  Wrench,
} from "@phosphor-icons/react";
import { errorMessage, type Provider } from "../bridge";
import { setModel, type AgentHost } from "../agent";
import { projectSession, type Phase } from "../agent-session";
import { modelRef, resolveModel, toPiModel } from "../providers";
import {
  applyThinking,
  contextUsage,
  type ContextUsage,
  effectiveThinkingLevel,
  formatTokens,
  parseThinkingLevel,
  supportedThinkingLevels,
} from "../agent-thinking";
import { beforeAppUpdate } from "../update-preparation";
import { IMAGE_TYPES, MAX_IMAGES, isAcceptedImage, prepareImage, type Attachment } from "../image-attachments";

type Block = {
  type: string;
  text?: string;
  thinking?: string;
  id?: string;
  name?: string;
  arguments?: Record<string, unknown>;
};
type Msg = AgentMessage & {
  role: string;
  content?: unknown;
  toolCallId?: string;
  isError?: boolean;
  stopReason?: string;
  errorMessage?: string;
};
type ToolState = "running" | "queued" | "success" | "error" | "skipped";
const phaseLabels: Record<Phase, string> = {
  idle: "",
  waiting: "正在等待模型…",
  thinking: "正在思考…",
  responding: "正在回复…",
  tools: "正在执行工具…",
  saving: "正在保存对话…",
  stopping: "正在停止…",
};
const toolLabels: Record<string, string> = {
  list_files: "列出文件",
  read_file: "读取",
  write_file: "写入",
  edit_file: "修改",
  delete_file: "删除",
  check_package: "检查程序包",
  push_to_device: "推送到设备",
  list_simulators: "列出模拟器",
  run_in_simulator: "在模拟器运行",
  inspect_simulator: "读取模拟器状态",
  press_simulator_button: "模拟器按键测试",
  update_project_brief: "更新项目说明",
  list_home: "查看目录",
  read_home: "读取项目外文件",
  write_home: "写入项目外文件",
  fetch_url: "打开网页",
  web_search: "联网搜索",
  gizos_docs_search: "搜索 GizOS 文档",
  gizos_docs_read: "读取 GizOS 文档",
};
const fileTools: Record<string, string> = {
  read_file: "读取",
  write_file: "写入",
  edit_file: "修改",
  delete_file: "删除",
  read_home: "读取",
  write_home: "写入",
};
/** "列出文件，写入 breathing_light.lua，检查程序包" — one phrase per kind of tool, in call order. */
function describeTools(tools: Block[]) {
  const order: string[] = [];
  const paths = new Map<string, string[]>();
  for (const b of tools) {
    const name = b.name ?? "";
    if (!paths.has(name)) {
      order.push(name);
      paths.set(name, []);
    }
    const path = typeof b.arguments?.path === "string" ? b.arguments.path : "";
    paths.get(name)!.push(path);
  }
  return order
    .map((name) => {
      const verb = fileTools[name] ? t(fileTools[name]) : undefined;
      if (!verb) return t(toolLabels[name] ?? name);
      const list = paths.get(name)!;
      const file = list[0]?.split("/").pop();
      return list.length === 1 && file
        ? `${verb} ${file}`
        : t("{action} {count} 个文件", { action: verb, count: list.length });
    })
    .join(t("，"));
}
function imagesOf(content: unknown) {
  return Array.isArray(content)
    ? (content as { type?: string; data?: unknown; mimeType?: unknown }[]).filter(
        (b): b is { type: "image"; data: string; mimeType: string } =>
          b?.type === "image" && typeof b.data === "string" && typeof b.mimeType === "string",
      )
    : [];
}
function textOf(content: unknown) {
  if (typeof content === "string") return content;
  return Array.isArray(content)
    ? (content as Block[])
        .filter((b) => b.type === "text")
        .map((b) => b.text)
        .join("")
    : "";
}

export function AgentPanel({
  host,
  providers,
  onOpenProviders,
  toolbar,
}: {
  host: AgentHost;
  providers: Provider[];
  onOpenProviders: () => void;
  /** Chat toolbar slot that receives the 清空对话 action. */
  toolbar?: HTMLElement | null;
}) {
  const { language } = useI18n();
  const session = useMemo(() => projectSession(host.projectId), [host.projectId]);
  useSyncExternalStore(session.subscribe, session.getVersion);
  const storageKey = `gizmoth.model.${host.projectId}`;
  const thinkingKey = `gizmoth.thinking.${host.projectId}`;
  const [ref, setRef] = useState(
    () => localStorage.getItem(storageKey) ?? localStorage.getItem("gizmoth.model") ?? undefined,
  );
  const [requestedThinking, setRequestedThinking] = useState(() =>
    parseThinkingLevel(localStorage.getItem(thinkingKey)),
  );
  // The draft lives in the session so it survives leaving the project.
  const [input, setInputState] = useState(() => session.draft.text);
  const [attachments, setAttachmentsState] = useState<Attachment[]>(() => session.draft.attachments);
  const draftRef = useRef({ text: input, attachments });
  const setInput = (next: string | ((draft: string) => string)) =>
    setInputState((draft) => {
      const value = typeof next === "function" ? next(draft) : next;
      draftRef.current = { ...draftRef.current, text: value };
      session.setDraft(draftRef.current);
      return value;
    });
  const setAttachments = (next: Attachment[] | ((items: Attachment[]) => Attachment[])) =>
    setAttachmentsState((items) => {
      const value = typeof next === "function" ? next(items) : next;
      draftRef.current = { ...draftRef.current, attachments: value };
      session.setDraft(draftRef.current);
      return value;
    });
  const [attaching, setAttaching] = useState(false);
  const [dragging, setDragging] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const [copied, setCopied] = useState("");
  const [confirmingRewind, setConfirmingRewind] = useState<number | null>(null);
  const [openGroups, setOpenGroups] = useState<ReadonlySet<string>>(() => new Set());
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const stickToBottom = useRef(true);
  const hostRef = useRef(host);
  hostRef.current = host;
  const providersRef = useRef(providers);
  providersRef.current = providers;
  const thinkingRef = useRef(requestedThinking);
  thinkingRef.current = requestedThinking;
  const resolved = resolveModel(providers, ref);
  const selectedModel = resolved ? toPiModel(resolved.provider, resolved.model) : undefined;
  const thinking = effectiveThinkingLevel(selectedModel, requestedThinking);
  const thinkingLevels = supportedThinkingLevels(selectedModel);
  const hasModels = providers.some((p) => p.models.length);
  const acceptsImages = !!selectedModel?.input.includes("image");
  const { agent, loaded, running, stopping, phase, currentTools } = session;
  const error = session.displayError;
  const attachmentName = (item: Attachment) => (item.nameKey ? t(item.nameKey, item.nameValues) : item.name);
  const setError = (message: string) => session.setError(message);
  const brief = session.brief;

  // The session outlives this view: leaving the page detaches it without stopping a run.
  useEffect(
    () =>
      session.attach({
        translate: t,
        onFilesChanged: (path) => hostRef.current.onFilesChanged(path),
        device: () => hostRef.current.device(),
        confirm: (title, body, labels) => hostRef.current.confirm(title, body, labels),
      }),
    [session],
  );
  useEffect(() => {
    stickToBottom.current = true;
  }, [session]);
  useEffect(() => {
    if (loaded && resolved)
      session.ensureAgent(() => providersRef.current, modelRef(resolved.provider, resolved.model), thinkingRef.current);
  }, [session, loaded, resolved]);
  // A failed send or a rewind hands text and images back, before the next paint.
  const restored = session.restored;
  useLayoutEffect(() => {
    if (!restored) return;
    setInputState(session.draft.text);
    setAttachmentsState(session.draft.attachments);
    draftRef.current = session.draft;
  }, [session, restored]);

  // Esc stops a run unless a dialog, menu or popover is taking the key.
  useEffect(() => {
    if (!running) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || event.isComposing) return;
      // Open dialogs and popovers (menus, the model picker and its submenu) handle Esc themselves.
      if (document.querySelector("dialog[open], :popover-open")) return;
      event.preventDefault();
      session.stop();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [running, session]);

  useEffect(() => {
    if (agent && resolved && !running) {
      setModel(agent, providers, modelRef(resolved.provider, resolved.model));
      applyThinking(agent, thinking);
    }
  }, [agent, providers, resolved, thinking, running]);

  useEffect(
    () =>
      beforeAppUpdate(async () => {
        if (input.trim()) throw new Error(t("助手输入框有未发送内容，请发送或保存后再安装更新。"));
      }),
    [input],
  );

  useLayoutEffect(() => {
    const field = inputRef.current;
    if (!field) return;
    field.style.height = "0px";
    field.style.height = `${Math.min(180, Math.max(28, field.scrollHeight))}px`;
  }, [input]);
  useEffect(() => {
    const field = inputRef.current;
    if (!field) return;
    let lastWidth = field.clientWidth;
    const observer = new ResizeObserver(() => {
      if (field.clientWidth === lastWidth) return;
      lastWidth = field.clientWidth;
      field.style.height = "0px";
      field.style.height = `${Math.min(180, Math.max(28, field.scrollHeight))}px`;
    });
    observer.observe(field);
    return () => observer.disconnect();
  }, []);

  const messages = (agent?.state.messages ?? []) as Msg[];
  const streaming = agent?.state.streamingMessage as Msg | undefined;
  // Shown only when the provider actually reported usage for this conversation.
  const usage = contextUsage(messages);
  const contextWindow = resolved?.model.contextWindow ?? 0;
  const results = useMemo(() => {
    const map = new Map<string, { isError: boolean; text: string }>();
    for (const m of messages)
      if (m.role === "toolResult" && m.toolCallId)
        map.set(m.toolCallId, { isError: !!m.isError, text: textOf(m.content) });
    return map;
    // The SDK mutates its transcript array in place on message_end.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agent, messages.length]);
  useEffect(() => {
    if (stickToBottom.current) scroller.current?.scrollTo({ top: scroller.current.scrollHeight });
  });

  async function send() {
    const text = input.trim();
    const sent = attachments;
    if (!agent || (!text && !sent.length) || running || attaching || agent.state.isStreaming) return;
    stickToBottom.current = true;
    setInputState("");
    setAttachmentsState([]);
    draftRef.current = { text: "", attachments: [] };
    await session.send(text, sent, thinkingRef.current);
  }
  async function attach(files: File[]) {
    const images = files.filter((file) => file.type.startsWith("image/"));
    if (!images.length) return;
    if (!acceptsImages) {
      setError(t("当前模型不支持图片输入，换一个支持图片的模型后再添加。"));
      return;
    }
    const room = MAX_IMAGES - attachments.length;
    if (room <= 0) {
      setError(t("每条消息最多 {count} 张图片。", { count: MAX_IMAGES }));
      return;
    }
    setAttaching(true);
    setError("");
    const problems: string[] = [];
    const added: Attachment[] = [];
    for (const file of images.slice(0, room)) {
      try {
        if (!isAcceptedImage(file.type))
          throw new Error(t("不支持 {name}，只能添加 PNG、JPEG、GIF 或 WebP 图片。", { name: file.name }));
        added.push(await prepareImage(file, t));
      } catch (e) {
        problems.push(errorMessage(e));
      }
    }
    if (images.length > room) problems.push(t("每条消息最多 {count} 张图片，多出的没有添加。", { count: MAX_IMAGES }));
    setAttachments((current) => [...current, ...added].slice(0, MAX_IMAGES));
    setAttaching(false);
    if (problems.length) setError(problems.join(" "));
    inputRef.current?.focus();
  }
  /** One conversation per project: clearing empties the chat and keeps the brief. */
  async function clear() {
    if (!agent || running || agent.state.isStreaming || !agent.state.messages.length) return;
    const ok = await host.confirm(
      t("清空对话"),
      brief
        ? t("清空这个项目的对话记录？项目说明会保留，助手之后会按它继续。")
        : t("清空这个项目的对话记录？项目还没有说明，助手之后不会记得之前聊过的内容。"),
      { confirm: t("清空"), cancel: t("取消"), danger: true },
    );
    if (!ok) return;
    setOpenGroups(new Set());
    await session.clear();
  }
  // A turn ends at the last assistant message before the next prompt (or the end, once idle).
  function isTurnEnd(index: number) {
    for (let j = index + 1; j < messages.length; j++) {
      if (messages[j].role === "user") return true;
      if (messages[j].role === "assistant") return false;
    }
    return !running;
  }
  const lastTurnEnd = (() => {
    for (let j = messages.length - 1; j >= 0; j--) if (messages[j].role === "assistant") return isTurnEnd(j) ? j : -1;
    return -1;
  })();
  function promptBefore(index: number) {
    for (let j = index; j >= 0; j--) if (messages[j].role === "user") return j;
    return -1;
  }
  function turnText(end: number) {
    const start = promptBefore(end) + 1;
    return messages
      .slice(start, end + 1)
      .filter((m) => m.role === "assistant")
      .map((m) => textOf(m.content).trim())
      .filter(Boolean)
      .join("\n\n");
  }
  async function copyText(key: string, text: string) {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // WebViews without clipboard permission still allow a user-initiated copy command.
      const field = document.createElement("textarea");
      field.value = text;
      field.style.position = "fixed";
      field.style.opacity = "0";
      document.body.append(field);
      field.select();
      const ok = document.execCommand("copy");
      field.remove();
      if (!ok) return setError(t("无法复制到剪贴板。"));
    }
    setCopied(key);
    window.setTimeout(() => setCopied((current) => (current === key ? "" : current)), 1500);
  }
  /** Confirmed inline under the message, then the prompt and everything after it go back to the input. */
  function rewind(index: number) {
    if (!agent || running) return;
    setConfirmingRewind(null);
    setOpenGroups(new Set());
    session.rewind(index);
    requestAnimationFrame(() => {
      const field = inputRef.current;
      if (!field) return;
      field.focus();
      field.setSelectionRange(field.value.length, field.value.length);
    });
  }
  function renderActions(kind: "user" | "assistant", index: number, m: Msg) {
    const key = `${kind}-${index}`;
    const text = kind === "user" ? textOf(m.content) : turnText(index);
    const at = typeof m.timestamp === "number" ? m.timestamp : 0;
    const prompt = kind === "assistant" ? promptBefore(index) : index;
    if (kind === "user" && confirmingRewind === index)
      return (
        <div
          className="msg-actions is-confirming"
          role="group"
          aria-label={t("确认回退")}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              event.stopPropagation();
              setConfirmingRewind(null);
            }
          }}
        >
          <span className="msg-rewind-text">
            {input.trim() || attachments.length
              ? t("回退到这条消息？之后的对话会删除，输入框内容会被替换，改过的文件不会撤销。")
              : t("回退到这条消息？之后的对话会删除，改过的文件不会撤销。")}
          </span>
          <button type="button" className="msg-confirm" onClick={() => setConfirmingRewind(null)}>
            {t("取消")}
          </button>
          <button
            type="button"
            className="msg-confirm danger"
            aria-label={t("确认回退")}
            autoFocus
            disabled={!agent || running}
            onClick={() => rewind(index)}
          >
            {t("回退")}
          </button>
        </div>
      );
    return (
      <div className="msg-actions" role="group" aria-label={kind === "user" ? t("消息操作") : t("回复操作")}>
        {at > 0 && (
          <time dateTime={new Date(at).toISOString()} title={new Date(at).toLocaleString(language)}>
            {relativeTime(at)}
          </time>
        )}
        {text && (
          <button
            type="button"
            className="msg-action"
            aria-label={copied === key ? t("已复制") : kind === "user" ? t("复制消息") : t("复制回复")}
            title={copied === key ? t("已复制") : t("复制")}
            onClick={() => void copyText(key, text)}
          >
            {copied === key ? <Check size={14} /> : <Copy size={14} />}
          </button>
        )}
        {kind === "user" && (
          <button
            type="button"
            className="msg-action"
            aria-label={t("回退到这里")}
            title={t("回退到这条消息，重新编辑")}
            disabled={!agent || running}
            onClick={() => setConfirmingRewind(index)}
          >
            <ArrowCounterClockwise size={14} />
          </button>
        )}
        {kind === "assistant" && index === lastTurnEnd && prompt >= 0 && (
          <button
            type="button"
            className="msg-action"
            aria-label={t("重新生成")}
            title={t("重新生成这条回复")}
            disabled={!agent || running}
            onClick={() => {
              setOpenGroups(new Set());
              stickToBottom.current = true;
              void session.retry(prompt, thinkingRef.current);
            }}
          >
            <ArrowClockwise size={14} />
          </button>
        )}
      </div>
    );
  }
  function toolState(b: Block, live: boolean): ToolState {
    const result = b.id ? results.get(b.id) : undefined;
    if (result) return result.isError ? "error" : "success";
    if (running && !!b.id && !!agent?.state.pendingToolCalls.has(b.id)) return "running";
    return live || (running && currentTools.has(b.id ?? "")) ? "queued" : "skipped";
  }
  function renderTool(b: Block, state: ToolState, key: number) {
    const result = b.id ? results.get(b.id) : undefined;
    const target = [b.arguments?.path, b.arguments?.url, b.arguments?.query].find(
      (value): value is string => typeof value === "string" && !!value,
    );
    const path = target ?? "";
    return (
      <div className={`tool ${state === "error" ? "failed" : ""}`} data-state={state} key={key} title={result?.text}>
        {state === "error" ? (
          <TriangleAlert size={12} />
        ) : state === "success" ? (
          <Check size={12} />
        ) : state === "running" ? (
          <LoaderCircle size={12} className="spin" />
        ) : state === "queued" ? (
          <Clock size={12} />
        ) : (
          <MinusCircle size={12} />
        )}
        <span>{t(toolLabels[b.name ?? ""] ?? b.name ?? "")}</span>
        {path && <code>{path}</code>}
        {state === "running" || state === "queued" || state === "skipped" ? (
          <span className="tool-state">
            {state === "running" ? t("执行中") : state === "queued" ? t("待执行") : t("未执行")}
          </span>
        ) : null}
        {state === "error" && <span className="tool-error">{result?.text}</span>}
      </div>
    );
  }
  function renderToolGroup(tools: Block[], key: string, live: boolean) {
    const states = tools.map((b) => toolState(b, live));
    const failed = states.filter((state) => state === "error").length;
    const runningIndex = states.indexOf("running");
    const group: ToolState =
      runningIndex >= 0 ? "running" : failed ? "error" : states.includes("queued") ? "queued" : "success";
    const text =
      runningIndex >= 0 ? t("正在{action}", { action: describeTools([tools[runningIndex]]) }) : describeTools(tools);
    const notRun = states.filter((state) => state === "skipped").length;
    return (
      <details
        className="agent-tool-group"
        data-state={group}
        key={key}
        open={openGroups.has(key)}
        onToggle={(event) => {
          const open = event.currentTarget.open;
          setOpenGroups((current) => {
            if (current.has(key) === open) return current;
            const next = new Set(current);
            if (open) next.add(key);
            else next.delete(key);
            return next;
          });
        }}
      >
        <summary title={text}>
          {group === "running" ? (
            <LoaderCircle size={13} className="spin" aria-hidden="true" />
          ) : group === "error" ? (
            <TriangleAlert size={13} aria-hidden="true" />
          ) : null}
          <span className="agent-tool-group-text">{text}</span>
          {failed > 0 && <span className="agent-tool-group-failed">{t("{count} 项失败", { count: failed })}</span>}
          {notRun > 0 && <span className="tool-state">{t("{count} 项未执行", { count: notRun })}</span>}
          <CaretRight size={11} className="caret" aria-hidden="true" />
        </summary>
        <div className="agent-tool-list">{tools.map((b, i) => renderTool(b, states[i], i))}</div>
      </details>
    );
  }
  function renderAssistant(m: Msg, key: string, live = false) {
    const blocks = (Array.isArray(m.content) ? m.content : []) as Block[];
    if (live && !blocks.length) return null;
    // Consecutive tool calls collapse into one summary line.
    const parts: (Block | Block[])[] = [];
    for (const b of blocks) {
      const last = parts[parts.length - 1];
      if (b.type === "toolCall" && Array.isArray(last)) last.push(b);
      else parts.push(b.type === "toolCall" ? [b] : b);
    }
    return (
      <div className="msg assistant" key={key}>
        {parts.map((b, i) => {
          // Keyed by the first call id so an expanded group survives the live → saved handoff.
          if (Array.isArray(b)) return renderToolGroup(b, b[0].id ?? `${key}-${i}`, live);
          if (b.type === "text" && b.text)
            return (
              <div className="msg-text" key={i}>
                {b.text}
              </div>
            );
          if (b.type === "thinking" && b.thinking)
            return (
              <details className="agent-thinking-block" open={live} key={i}>
                <summary>
                  <Brain size={14} aria-hidden="true" />
                  {live && phase === "thinking" ? t("思考中") : t("思考过程")}
                </summary>
                <div className="msg-thinking">{b.thinking}</div>
              </details>
            );
          return null;
        })}
        {m.stopReason === "error" && <div className="msg-error">{m.errorMessage || t("请求失败")}</div>}
        {m.stopReason === "aborted" && <p className="msg-note">{t("已停止")}</p>}
      </div>
    );
  }

  return (
    <aside className="agent" aria-busy={running}>
      <div
        className="agent-log"
        ref={scroller}
        onScroll={() => {
          const el = scroller.current;
          if (el) stickToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 72;
        }}
      >
        {!hasModels ? (
          <div className="agent-empty">
            <Sparkles size={20} />
            <p>{t("添加一个模型 Provider 后，就能让助手帮你写程序。")}</p>
            <Button onClick={onOpenProviders}>{t("配置 Provider")}</Button>
          </div>
        ) : !messages.length && !streaming ? (
          <div className="agent-empty">
            <Sparkles size={20} />
            <p>{t("告诉助手你想让设备做什么。它只能修改当前项目文件，推送到设备前会先征求你同意。")}</p>
          </div>
        ) : null}
        {messages.map((m, i) =>
          m.role === "user" ? (
            <div className="msg-turn user" key={i}>
              <div className="msg user">
                {imagesOf(m.content).length > 0 && (
                  <div className="msg-images">
                    {imagesOf(m.content).map((image, j) => (
                      <img
                        key={j}
                        src={`data:${image.mimeType};base64,${image.data}`}
                        alt={t("附加的图片 {count}", { count: j + 1 })}
                      />
                    ))}
                  </div>
                )}
                {textOf(m.content) && <div className="msg-user-text">{textOf(m.content)}</div>}
              </div>
              {renderActions("user", i, m)}
            </div>
          ) : m.role === "assistant" ? (
            isTurnEnd(i) ? (
              <div className={`msg-turn assistant${i === lastTurnEnd ? " is-last" : ""}`} key={i}>
                {renderAssistant(m, String(i))}
                {renderActions("assistant", i, m)}
              </div>
            ) : (
              renderAssistant(m, String(i))
            )
          ) : null,
        )}
        {streaming?.role === "assistant" && renderAssistant(streaming, "live", true)}
        {running && (
          <div className="agent-run-status" role="status">
            <span>{t(phaseLabels[phase])}</span>
            {!stopping && phase !== "saving" && <kbd className="agent-stop-hint">{t("Esc 停止")}</kbd>}
          </div>
        )}
      </div>
      {error && (
        <p className="error agent-error" role="alert">
          {error}
        </p>
      )}
      {toolbar &&
        createPortal(
          <IconButton
            title={t("清空对话（保留项目说明）")}
            aria-label={t("清空对话")}
            disabled={!agent || running || !messages.length}
            onClick={() => void clear()}
          >
            <Broom size={17} />
          </IconButton>,
          toolbar,
        )}
      <form
        className="composer agent-composer"
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <div
          className={`agent-composer-input${dragging ? " is-dragging" : ""}`}
          onDragOver={(e) => {
            if (![...e.dataTransfer.types].includes("Files")) return;
            e.preventDefault();
            e.dataTransfer.dropEffect = acceptsImages ? "copy" : "none";
            setDragging(true);
          }}
          onDragLeave={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDragging(false);
          }}
          onDrop={(e) => {
            if (!e.dataTransfer.files.length) return;
            e.preventDefault();
            setDragging(false);
            void attach([...e.dataTransfer.files]);
          }}
        >
          {attachments.length > 0 && (
            <ul className="agent-attachments" aria-label={t("已添加的图片")}>
              {attachments.map((item) => (
                <li key={item.id} title={attachmentName(item)}>
                  <img src={`data:${item.mimeType};base64,${item.data}`} alt={attachmentName(item)} />
                  <button
                    type="button"
                    className="agent-attachment-remove"
                    aria-label={t("移除图片 {name}", { name: attachmentName(item) })}
                    onClick={() => {
                      setAttachments((current) => current.filter((other) => other.id !== item.id));
                      inputRef.current?.focus();
                    }}
                  >
                    <X size={10} weight="bold" />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <IconButton
            className="agent-attach"
            aria-label={t("添加图片")}
            title={
              acceptsImages
                ? t("添加图片（最多 {count} 张，也可以粘贴或拖入）", { count: MAX_IMAGES })
                : t("当前模型不支持图片输入")
            }
            disabled={!agent || !loaded || !acceptsImages || attaching || attachments.length >= MAX_IMAGES}
            onClick={() => fileInput.current?.click()}
          >
            {attaching ? <LoaderCircle size={16} className="spin" /> : <Paperclip size={17} />}
          </IconButton>
          <input
            ref={fileInput}
            type="file"
            accept={IMAGE_TYPES.join(",")}
            multiple
            hidden
            tabIndex={-1}
            onChange={(e) => {
              const files = [...(e.target.files ?? [])];
              e.target.value = "";
              void attach(files);
            }}
          />
          <textarea
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onPaste={(e) => {
              const files = [...e.clipboardData.files].filter((file) => file.type.startsWith("image/"));
              if (!files.length) return;
              e.preventDefault();
              void attach(files);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                void send();
              }
            }}
            placeholder={hasModels ? t("描述你想要的功能…") : t("先配置模型 Provider")}
            aria-label={t("消息")}
            disabled={!agent || !loaded}
            rows={1}
          />
          {running ? (
            // No stop button: Esc stops (see the key handler); the spinner only shows progress.
            <span
              className="send is-running"
              title={stopping ? t("正在停止…") : t("生成中，按 Esc 停止")}
              aria-hidden="true"
            >
              <LoaderCircle size={16} className="spin" />
            </span>
          ) : (
            <button
              className="send"
              title={t("发送（Enter）")}
              aria-label={t("发送")}
              disabled={
                !agent ||
                !loaded ||
                attaching ||
                (!input.trim() && !attachments.length) ||
                (attachments.length > 0 && !acceptsImages)
              }
            >
              <ArrowElbowDownLeft size={17} />
            </button>
          )}
        </div>
        <div className="agent-composer-footer">
          <AgentModelPicker
            providers={providers}
            value={resolved ? modelRef(resolved.provider, resolved.model) : ""}
            onChange={(value) => {
              setRef(value);
              localStorage.setItem(storageKey, value);
              localStorage.setItem("gizmoth.model", value);
              if (agent) {
                setModel(agent, providers, value);
                applyThinking(agent, thinkingRef.current);
              }
            }}
            disabled={!hasModels || !loaded || running}
            thinking={thinking}
            thinkingLevels={thinkingLevels}
            onThinkingChange={(value) => {
              setRequestedThinking(value);
              thinkingRef.current = value;
              localStorage.setItem(thinkingKey, value);
              if (agent) applyThinking(agent, value);
            }}
          />
          {usage && contextWindow > 0 && (
            <ContextMeter
              usage={usage}
              total={contextWindow}
              onReset={running || !agent ? undefined : () => void clear()}
            />
          )}
        </div>
      </form>
      {!loaded && hasModels && (
        <div className="agent-loading">
          <Wrench size={12} /> {t("正在准备助手…")}
        </div>
      )}
    </aside>
  );
}

/** Context-window usage reported by the model: a 16px ring that opens the exact numbers. */
function ContextMeter({ usage, total, onReset }: { usage: ContextUsage; total: number; onReset?: () => void }) {
  useI18n();
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const popover = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<CSSProperties>({});
  const ratio = Math.min(1, usage.total / total);
  const percent = Math.round(ratio * 100);
  const level = ratio >= 0.9 ? "high" : ratio >= 0.75 ? "warn" : undefined;
  const summary = t("{used} / {total}（{percent}%）", {
    used: formatTokens(usage.total),
    total: formatTokens(total),
    percent,
  });
  const r = 6;
  const c = 2 * Math.PI * r;
  const rows = [
    [t("输入"), usage.input],
    [t("缓存读取"), usage.cacheRead],
    [t("缓存写入"), usage.cacheWrite],
    [t("输出"), usage.output],
  ].filter(([, value]) => (value as number) > 0) as [string, number][];

  function show() {
    const rect = trigger.current?.getBoundingClientRect();
    if (!rect) return;
    const width = Math.min(248, window.innerWidth - 24);
    setPosition({
      left: Math.max(12, Math.min(rect.right - width, window.innerWidth - width - 12)),
      bottom: window.innerHeight - rect.top + 8,
      width,
    });
    popover.current?.showPopover();
  }
  useEffect(() => {
    if (!open) return;
    const dismiss = () => popover.current?.hidePopover();
    window.addEventListener("resize", dismiss);
    return () => window.removeEventListener("resize", dismiss);
  }, [open]);

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className="agent-context-meter"
        aria-label={t("上下文用量 {summary}", { summary })}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={`${id}-context`}
        title={t("上下文 {summary}", { summary })}
        data-level={level}
        onClick={() => (open ? popover.current?.hidePopover() : show())}
      >
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
          <circle cx="8" cy="8" r={r} fill="none" strokeWidth="2" className="track" />
          <circle
            cx="8"
            cy="8"
            r={r}
            fill="none"
            strokeWidth="2"
            strokeLinecap="round"
            strokeDasharray={`${c * ratio} ${c}`}
            transform="rotate(-90 8 8)"
            className="value"
          />
        </svg>
      </button>
      <div
        ref={popover}
        id={`${id}-context`}
        popover="auto"
        role="dialog"
        aria-label={t("上下文窗口")}
        className="agent-context-popover"
        style={position}
        onToggle={(event) => setOpen(event.newState === "open")}
      >
        <div className="agent-context-head">
          <span>{t("上下文窗口")}</span>
          <span className="agent-context-total">{summary}</span>
        </div>
        <div
          className="agent-context-bar"
          role="meter"
          aria-label={t("上下文用量")}
          aria-valuemin={0}
          aria-valuemax={total}
          aria-valuenow={Math.min(usage.total, total)}
          aria-valuetext={summary}
          data-level={level}
        >
          <span style={{ width: `${Math.max(ratio * 100, 1)}%` }} />
        </div>
        {rows.length > 0 && (
          <dl className="agent-context-rows">
            {rows.map(([label, value]) => (
              <div key={label}>
                <dt>{label}</dt>
                <dd>{formatTokens(value)}</dd>
              </div>
            ))}
          </dl>
        )}
        <p className="agent-context-note">
          {level
            ? t("快满了。清空对话可以腾出空间，项目说明会保留。")
            : t("按最近一次回复统计，包含项目说明、对话、工具结果和图片。")}
        </p>
        {level && onReset && (
          <Button
            controlSize="sm"
            onClick={() => {
              popover.current?.hidePopover();
              onReset();
            }}
          >
            {t("清空对话…")}
          </Button>
        )}
      </div>
    </>
  );
}
