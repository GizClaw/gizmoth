import { useEffect, useMemo, useRef, useState } from "react";
import type { Agent, AgentMessage } from "@mariozechner/pi-agent-core";
import { ArrowUp, Check, LoaderCircle, RotateCcw, Sparkles, Square, TriangleAlert, Wrench } from "lucide-react";
import { bridge, errorMessage, native, type Provider } from "../bridge";
import { createAgent, setModel, storableMessages, type AgentHost } from "../agent";
import { modelRef, resolveModel } from "../providers";

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
  if (Array.isArray(content))
    return (content as Block[])
      .filter((b) => b.type === "text")
      .map((b) => b.text)
      .join("");
  return "";
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
  const [, setTick] = useState(0);
  const [input, setInput] = useState("");
  const [error, setError] = useState("");
  const [loaded, setLoaded] = useState(false);
  const scroller = useRef<HTMLDivElement>(null);
  const hostRef = useRef(host);
  hostRef.current = host;
  const resolved = resolveModel(providers, ref);
  const hasModels = providers.some((p) => p.models.length);

  // One agent per project; history comes from disk.
  useEffect(() => {
    let cancelled = false;
    let unsubscribe = () => {};
    setLoaded(false);
    setAgent(undefined);
    void (async () => {
      let messages: AgentMessage[] = [];
      try {
        const raw = native ? await bridge.chatLoad(host.projectId) : null;
        if (raw) messages = JSON.parse(raw) as AgentMessage[];
      } catch (e) {
        setError(`无法恢复对话：${errorMessage(e)}`);
      }
      if (cancelled) return;
      const proxyHost: AgentHost = {
        projectId: host.projectId,
        onFilesChanged: (p) => hostRef.current.onFilesChanged(p),
        device: () => hostRef.current.device(),
        confirm: (t, b) => hostRef.current.confirm(t, b),
      };
      const next = createAgent(
        proxyHost,
        providers,
        resolved ? modelRef(resolved.provider, resolved.model) : undefined,
        messages,
      );
      if (next) {
        unsubscribe = next.subscribe(async (event) => {
          setTick((t) => t + 1);
          if (event.type === "agent_end" && native)
            await bridge
              .chatSave(host.projectId, JSON.stringify(storableMessages(next.state.messages)))
              .catch((e) => setError(errorMessage(e)));
        });
      }
      setAgent(next);
      setLoaded(true);
    })();
    return () => {
      cancelled = true;
      unsubscribe();
    };
    // Recreate only when the project changes or the first model becomes available.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host.projectId, !!resolved]);

  useEffect(() => {
    if (agent && resolved) setModel(agent, providers, modelRef(resolved.provider, resolved.model));
  }, [agent, providers, resolved]);

  const messages = (agent?.state.messages ?? []) as Msg[];
  const streaming = agent?.state.streamingMessage as Msg | undefined;
  const running = agent?.state.isStreaming ?? false;
  const results = useMemo(() => {
    const map = new Map<string, { isError: boolean; text: string }>();
    for (const m of messages)
      if (m.role === "toolResult" && m.toolCallId)
        map.set(m.toolCallId, { isError: !!m.isError, text: textOf(m.content) });
    return map;
  }, [messages.length]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    scroller.current?.scrollTo({ top: scroller.current.scrollHeight });
  });

  async function send() {
    const text = input.trim();
    if (!agent || !text || running) return;
    setInput("");
    setError("");
    try {
      await agent.prompt(text);
      const last = agent.state.messages.at(-1) as Msg | undefined;
      if (last?.role === "assistant" && last.stopReason === "error") setError(last.errorMessage || "模型请求失败");
    } catch (e) {
      setError(errorMessage(e));
    }
  }

  function reset() {
    if (!agent || running) return;
    agent.reset();
    setTick((t) => t + 1);
    if (native) void bridge.chatSave(host.projectId, "[]");
  }

  function renderAssistant(m: Msg, key: string, live = false) {
    const blocks = (Array.isArray(m.content) ? m.content : []) as Block[];
    return (
      <div className="msg assistant" key={key}>
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
                ) : (
                  <LoaderCircle size={12} className="spin" />
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
          disabled={!hasModels || running}
          aria-label="模型"
        >
          {!hasModels && <option value="">未配置模型</option>}
          {providers
            .filter((p) => p.models.length)
            .map((p) => (
              <optgroup key={p.id} label={p.name}>
                {p.models.map((m) => (
                  <option key={m.id} value={modelRef(p, m)}>
                    {m.name}
                  </option>
                ))}
              </optgroup>
            ))}
        </select>
        <button
          className="icon-button"
          title="新对话"
          aria-label="新对话"
          disabled={!agent || running || !messages.length}
          onClick={reset}
        >
          <RotateCcw size={14} />
        </button>
      </div>
      <div className="agent-log" ref={scroller}>
        {!hasModels ? (
          <div className="agent-empty">
            <Sparkles size={20} />
            <p>添加一个模型 Provider 后，就能让助手帮你写程序。</p>
            <button className="button" onClick={onOpenProviders}>
              配置 Provider
            </button>
          </div>
        ) : !messages.length && !streaming ? (
          <div className="agent-empty">
            <Sparkles size={20} />
            <p>告诉助手你想让设备做什么。它会读写项目文件、检查程序包，推送前会先征求你同意。</p>
          </div>
        ) : null}
        {messages.map((m, i) =>
          m.role === "user" ? (
            <div className="msg user" key={i}>
              {textOf(m.content)}
            </div>
          ) : m.role === "assistant" ? (
            renderAssistant(m, String(i))
          ) : null,
        )}
        {streaming && streaming.role === "assistant" && renderAssistant(streaming, "live", true)}
        {running && !streaming && (
          <div className="msg assistant">
            <LoaderCircle size={14} className="spin muted" />
          </div>
        )}
      </div>
      {error && <p className="error agent-error">{error}</p>}
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
          disabled={!agent || !loaded}
          rows={3}
        />
        {running ? (
          <button type="button" className="send" aria-label="停止" onClick={() => agent?.abort()}>
            <Square size={12} />
          </button>
        ) : (
          <button className="send" aria-label="发送" disabled={!agent || !input.trim()}>
            <ArrowUp size={15} />
          </button>
        )}
      </form>
      {!loaded && hasModels && (
        <div className="agent-loading">
          <Wrench size={12} /> 正在准备助手…
        </div>
      )}
    </aside>
  );
}
