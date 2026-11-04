import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Agent, AgentMessage } from "@mariozechner/pi-agent-core";
import {
  ArrowUp,
  Check,
  ChevronUp,
  LoaderCircle,
  RotateCcw,
  Sparkles,
  Square,
  TriangleAlert,
  Wrench,
} from "lucide-react";
import { bridge, errorMessage, native, type ChatEntry, type ChatState, type Provider } from "../bridge";
import { createAgent, createTools, setModel, summarizeConversation, systemPrompt, type AgentHost } from "../agent";
import { ChatSession, contextBudget, estimateTokens } from "../chat-context";
import { modelRef, pickedModels, resolveModel } from "../providers";
import { Dialog } from "../ui";

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
const toolLabels: Record<string, string> = {
  list_files: "列出文件",
  read_file: "读取",
  write_file: "写入",
  edit_file: "修改",
  delete_file: "删除",
  check_package: "检查程序包",
  push_to_device: "推送到设备",
};
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
}: {
  host: AgentHost;
  providers: Provider[];
  onOpenProviders: () => void;
}) {
  const storageKey = `gizmoth.model.${host.projectId}`;
  const [ref, setRef] = useState(
    () => localStorage.getItem(storageKey) ?? localStorage.getItem("gizmoth.model") ?? undefined,
  );
  const [agent, setAgent] = useState<Agent>();
  const [session, setSession] = useState<ChatSession>();
  const [chat, setChat] = useState<ChatState>();
  const [entries, setEntries] = useState<ChatEntry[]>([]);
  const [, setTick] = useState(0);
  const [input, setInput] = useState("");
  const [error, setError] = useState("");
  const [loaded, setLoaded] = useState(false);
  const [paging, setPaging] = useState(false);
  const [compacting, setCompacting] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [showSummary, setShowSummary] = useState(false);
  const [reload, setReload] = useState(0);
  const scroller = useRef<HTMLDivElement>(null);
  const stickToEnd = useRef(true);
  const anchor = useRef<{ seq: string; top: number } | null>(null);
  const sending = useRef(false);
  const hostRef = useRef(host);
  hostRef.current = host;
  const resolved = useMemo(() => resolveModel(providers, ref), [providers, ref]);
  const hasModels = providers.some((p) => pickedModels(p).length);

  // UI history is independent of the model context, and opens on one small tail page.
  useEffect(() => {
    let gone = false;
    setLoaded(false);
    setSession(undefined);
    setChat(undefined);
    setEntries([]);
    setError("");
    stickToEnd.current = true;
    if (!native) {
      setLoaded(true);
      return;
    }
    void (async () => {
      try {
        const initial = await bridge.chatState(host.projectId);
        const page = await bridge.chatPage(host.projectId);
        if (initial.epoch !== page.epoch) throw new Error("对话已更新，请重新打开项目。");
        if (gone) return;
        const state = { ...initial, total: page.total };
        const next = new ChatSession(
          {
            page: (options) => bridge.chatPage(host.projectId, options),
            append: (epoch, expected, messages) => bridge.chatAppend(host.projectId, epoch, expected, messages),
            checkpoint: (epoch, value) => bridge.chatCheckpoint(host.projectId, epoch, value),
          },
          state,
          (entry) => {
            if (gone) return;
            setEntries((all) => {
              // When browsing older pages, new records stay available via “回到最新”.
              if (all.length && all.at(-1)!.seq < entry.seq - 1) return all;
              return [...all, entry].slice(-200);
            });
          },
          (value) => {
            if (!gone) setChat(value);
          },
        );
        setEntries(page.entries);
        setChat(state);
        setSession(next);
      } catch (e) {
        if (!gone) setError(`无法恢复对话：${errorMessage(e)}`);
      } finally {
        if (!gone) setLoaded(true);
      }
    })();
    return () => {
      gone = true;
    };
  }, [host.projectId, reload]);

  useEffect(() => {
    if (!session || !resolved) {
      setAgent(undefined);
      return;
    }
    let gone = false;
    const proxyHost: AgentHost = {
      projectId: host.projectId,
      onFilesChanged: (path) => hostRef.current.onFilesChanged(path),
      device: () => hostRef.current.device(),
      confirm: (title, body) => hostRef.current.confirm(title, body),
    };
    const overhead =
      estimateTokens(systemPrompt) +
      estimateTokens(
        createTools(proxyHost).map(({ name, description, parameters }) => ({ name, description, parameters })),
      );
    const next = createAgent(
      proxyHost,
      providers,
      modelRef(resolved.provider, resolved.model),
      [],
      async (_messages, signal) => {
        const budget = contextBudget(next!.state.model, overhead);
        return session.forModel(
          budget,
          async (previous, transcript, maxTokens, abort) => {
            if (!gone) setCompacting(true);
            try {
              return await summarizeConversation(next!.state.model, previous, transcript, maxTokens, abort);
            } catch (e) {
              throw new Error(`上下文整理未完成，完整历史已保留：${errorMessage(e)}`);
            } finally {
              if (!gone) setCompacting(false);
            }
          },
          signal,
        );
      },
    );
    if (!next) return;
    const unsubscribe = next.subscribe(async (event) => {
      try {
        if (event.type === "message_end") await session.record([event.message]);
        // Runtime failures can emit agent_end without a preceding message_end.
        if (event.type === "agent_end") await session.record(event.messages);
      } catch (e) {
        if (!gone) setError(`对话未保存：${errorMessage(e)}`);
        throw e;
      }
      if (!gone) setTick((t) => t + 1);
    });
    setAgent(next);
    return () => {
      gone = true;
      next.abort();
      // Keep the persistence listener until the aborted run finishes its final record.
      void next.waitForIdle().finally(unsubscribe);
    };
    // Models can change without reopening history; an unavailable selection leaves the composer disabled.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session, !!resolved]);

  useEffect(() => {
    if (agent && resolved) setModel(agent, providers, modelRef(resolved.provider, resolved.model));
  }, [agent, providers, resolved]);
  const streaming = agent?.state.streamingMessage as Msg | undefined;
  const running = agent?.state.isStreaming ?? false;
  const results = useMemo(() => {
    const map = new Map<string, { isError: boolean; text: string }>();
    for (const entry of entries) {
      const m = entry.message as Msg;
      if (m.role === "toolResult" && m.toolCallId)
        map.set(m.toolCallId, { isError: !!m.isError, text: textOf(m.content) });
    }
    return map;
  }, [entries]);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    if (anchor.current) {
      const node = el.querySelector<HTMLElement>(`[data-chat-seq="${anchor.current.seq}"]`);
      if (node) el.scrollTop += node.getBoundingClientRect().top - anchor.current.top;
      anchor.current = null;
    } else if (stickToEnd.current) el.scrollTop = el.scrollHeight;
  });

  async function send() {
    const text = input.trim();
    if (!agent || !session || !text || sending.current || running) return;
    sending.current = true;
    setInput("");
    setError("");
    stickToEnd.current = true;
    try {
      await session.flush();
      await agent.prompt(text);
      const last = agent.state.messages.at(-1) as Msg | undefined;
      if (last?.role === "assistant" && last.stopReason === "error") setError(last.errorMessage || "模型请求失败");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      // The UI uses paged records; future model requests restore from the checkpoint/tail.
      agent.state.messages = [];
      sending.current = false;
      setTick((t) => t + 1);
    }
  }

  async function pageHistory(latest = false) {
    if (!session || paging) return;
    setPaging(true);
    setError("");
    try {
      const page = await bridge.chatPage(host.projectId, latest ? {} : { before: entries[0]?.seq });
      if (page.epoch !== session.state.epoch) throw new Error("对话已被清空，请重新打开项目。");
      if (latest) {
        stickToEnd.current = true;
        setEntries(page.entries);
      } else {
        const el = scroller.current;
        const node =
          el &&
          [...el.querySelectorAll<HTMLElement>("[data-chat-seq]")].find(
            (item) => item.getBoundingClientRect().bottom > el.getBoundingClientRect().top,
          );
        if (node) anchor.current = { seq: node.dataset.chatSeq!, top: node.getBoundingClientRect().top };
        stickToEnd.current = false;
        setEntries((current) => [...page.entries, ...current].slice(0, 200));
      }
      setChat((current) => (current ? { ...current, total: page.total } : current));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setPaging(false);
    }
  }

  async function reset() {
    if (!session || running || sending.current || clearing) return;
    if (!(await hostRef.current.confirm("清空项目对话", "清除这个项目的全部对话和上下文摘要？项目文件不受影响。")))
      return;
    setClearing(true);
    setError("");
    try {
      await session.flush();
      await bridge.chatReset(host.projectId, session.state.epoch);
      setInput("");
      setReload((n) => n + 1);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setClearing(false);
    }
  }

  function renderAssistant(m: Msg, key: string, live = false) {
    const blocks = (Array.isArray(m.content) ? m.content : []) as Block[];
    return (
      <div className="msg assistant" key={key} data-chat-seq={live ? undefined : key}>
        {blocks.map((b, i) => {
          if (b.type === "text" && b.text)
            return (
              <div className="msg-text" key={i}>
                {b.text}
              </div>
            );
          if (b.type === "thinking" && b.thinking)
            return (
              <div className="msg-thinking" key={i}>
                {b.thinking}
              </div>
            );
          if (b.type === "toolCall") {
            const result = b.id ? results.get(b.id) : undefined;
            const path = typeof b.arguments?.path === "string" ? b.arguments.path : "";
            return (
              <div className={`tool ${result?.isError ? "failed" : ""}`} key={i} title={result?.text}>
                {result ? (
                  result.isError ? (
                    <TriangleAlert size={12} />
                  ) : (
                    <Check size={12} />
                  )
                ) : live || running ? (
                  <LoaderCircle size={12} className="spin" />
                ) : (
                  <Wrench size={12} />
                )}
                <span>{toolLabels[b.name ?? ""] ?? b.name}</span>
                {path && <code>{path}</code>}
                {result?.isError && <span className="tool-error">{result.text}</span>}
              </div>
            );
          }
          return null;
        })}
        {live && !blocks.length && <LoaderCircle size={14} className="spin muted" />}
        {m.stopReason === "error" && <div className="msg-error">{m.errorMessage || "请求失败"}</div>}
      </div>
    );
  }

  return (
    <aside className="agent">
      <div className="agent-head">
        <Sparkles size={14} />
        <select
          value={resolved ? modelRef(resolved.provider, resolved.model) : ""}
          onChange={(e) => {
            setRef(e.target.value);
            localStorage.setItem(storageKey, e.target.value);
            localStorage.setItem("gizmoth.model", e.target.value);
          }}
          disabled={!hasModels || running || clearing}
          aria-label="模型"
        >
          {!resolved && <option value="">{hasModels ? "请选择模型" : "未选择开发模型"}</option>}
          {providers
            .filter((p) => pickedModels(p).length)
            .map((p) => (
              <optgroup key={p.id} label={p.name}>
                {pickedModels(p).map((m) => (
                  <option key={m.id} value={modelRef(p, m)}>
                    {m.name}
                  </option>
                ))}
              </optgroup>
            ))}
        </select>
        <button
          className="icon-button"
          title="清空对话"
          aria-label="清空对话"
          disabled={!chat?.total || running || clearing}
          onClick={reset}
        >
          <RotateCcw size={14} />
        </button>
      </div>
      {(compacting || chat?.checkpoint) && (
        <div className="agent-context" role="status">
          {compacting ? (
            <>
              <LoaderCircle size={12} className="spin" />
              正在整理上下文…
            </>
          ) : (
            <>
              <Check size={12} />
              <span>已整理早期上下文，完整历史保留</span>
              <button className="text-button" onClick={() => setShowSummary(true)}>
                摘要
              </button>
            </>
          )}
        </div>
      )}
      <div
        className="agent-log"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget;
          stickToEnd.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
        }}
      >
        {!!entries.length && entries[0].seq > 1 && (
          <button className="history-page" disabled={paging} onClick={() => pageHistory()}>
            {paging ? <LoaderCircle size={12} className="spin" /> : <ChevronUp size={12} />}更早的记录
          </button>
        )}
        {!loaded && <p className="muted small-text">正在读取对话…</p>}
        {loaded && !entries.length && !streaming && (
          <div className="agent-empty">
            <Sparkles size={20} />
            <p>
              {hasModels
                ? "告诉助手你想让设备做什么。它会读写项目文件、检查程序包，推送前会先征求你同意。"
                : "先在 Provider 中勾选开发模型，再让助手帮你写程序。"}
            </p>
            {!hasModels && (
              <button className="button" onClick={onOpenProviders}>
                配置 Provider
              </button>
            )}
          </div>
        )}
        {entries.map(({ seq, message }) =>
          message.role === "user" ? (
            <div className="msg user" key={seq} data-chat-seq={seq}>
              {textOf((message as Msg).content)}
            </div>
          ) : message.role === "assistant" ? (
            renderAssistant(message as Msg, String(seq))
          ) : null,
        )}
        {entries.length > 0 && chat && entries.at(-1)!.seq < chat.total && (
          <button className="history-page" disabled={paging} onClick={() => pageHistory(true)}>
            回到最新记录
          </button>
        )}
        {streaming?.role === "assistant" && renderAssistant(streaming, "live", true)}
        {running && !streaming && !compacting && (
          <div className="msg assistant">
            <LoaderCircle size={14} className="spin muted" />
          </div>
        )}
      </div>
      {error && (
        <p className="error agent-error" role="alert">
          {error}
        </p>
      )}
      <form
        className="composer"
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void send();
            }
          }}
          placeholder={hasModels ? "描述你想要的功能…" : "先配置模型 Provider"}
          disabled={!agent || !loaded || clearing}
          rows={3}
        />
        {running ? (
          <button type="button" className="send" aria-label="停止" onClick={() => agent?.abort()}>
            <Square size={12} />
          </button>
        ) : (
          <button className="send" aria-label="发送" disabled={!agent || !loaded || clearing || !input.trim()}>
            <ArrowUp size={15} />
          </button>
        )}
      </form>
      {showSummary && chat?.checkpoint && (
        <Dialog title="项目上下文摘要" onClose={() => setShowSummary(false)}>
          <p className="dialog-text">摘要覆盖前 {chat.checkpoint.through} 条记录。完整消息仍可在对话中分页查看。</p>
          <div className="summary-text">{chat.checkpoint.summary}</div>
        </Dialog>
      )}
    </aside>
  );
}
