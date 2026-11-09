import type { Agent, AgentMessage } from "@earendil-works/pi-agent-core";
import { applyThinking, type Effort } from "./agent-thinking.ts";
import {
  bridge,
  defaultPermissions,
  errorMessage,
  native,
  type AgentPermissions,
  type Device,
  type Provider,
} from "./bridge.ts";
import {
  createAgent,
  parseChat,
  serializeChat,
  systemPromptWith,
  type AgentHost,
  type ConfirmLabels,
} from "./agent.ts";
import type { Attachment } from "./image-attachments.ts";
import { beforeAppUpdate } from "./update-preparation.ts";
import { updateAgentPrompt } from "./agent-transcript.ts";
import { cancelAgentSimulatorRuns } from "./simulator-window";

export type Phase = "idle" | "waiting" | "thinking" | "responding" | "tools" | "saving" | "stopping";

/** Callbacks from the project view that is currently on screen, if any. */
export interface SessionView {
  onFilesChanged: (path?: string) => void;
  device: () => Device | undefined;
  confirm: (title: string, body: string, labels?: ConfirmLabels) => Promise<boolean>;
  translate?: AgentHost["translate"];
}

export interface Draft {
  text: string;
  attachments: Attachment[];
}

/**
 * One conversation per project, owned outside React so it keeps running and
 * keeps saving when the user leaves the project page. Every finished message is
 * saved immediately, so quitting mid-run keeps the prompt and completed steps.
 */
export class ProjectSession {
  readonly projectId: string;
  agent?: Agent;
  loaded = false;
  brief = "";
  permissions: AgentPermissions = defaultPermissions;
  running = false;
  stopping = false;
  phase: Phase = "idle";
  error = "";
  /** Draft kept across navigation; a failed send puts its text and images back here. */
  draft: Draft = { text: "", attachments: [] };
  /** Bumped when a failed send restored the draft, so the view can pick it up. */
  restored = 0;
  readonly currentTools = new Set<string>();
  version = 0;

  private messages: AgentMessage[] = [];
  private view?: SessionView;
  private lastDevice?: Device;
  private lastConfirm?: SessionView["confirm"];
  private lastTranslate?: SessionView["translate"];
  private appError?: { message: string; values: Record<string, string | number>; original: string };
  private listeners = new Set<() => void>();
  private saves: Promise<void> = Promise.resolve();
  private disposed = false;
  private unsubscribe = () => {};
  private loading: Promise<void>;
  private creatingAgent?: Promise<Agent | undefined>;

  constructor(projectId: string) {
    this.projectId = projectId;
    this.loading = this.load();
  }

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  getVersion = () => this.version;

  private notify() {
    this.version++;
    for (const listener of this.listeners) listener();
  }

  private displayText(message: string, values: Record<string, string | number> = {}) {
    return (
      (this.view?.translate ?? this.lastTranslate)?.(message, values) ??
      message.replace(/\{(\w+)\}/g, (token, key: string) => (Object.hasOwn(values, key) ? String(values[key]) : token))
    );
  }

  /** Keep the source error and diagnostics intact; localize only its display wrapper. */
  get displayError() {
    const source = this.appError;
    return source && source.original === this.error ? this.displayText(source.message, source.values) : this.error;
  }

  private setAppError(message: string, values: Record<string, string | number> = {}) {
    this.error = message.replace(/\{(\w+)\}/g, (token, key: string) =>
      Object.hasOwn(values, key) ? String(values[key]) : token,
    );
    this.appError = { message, values, original: this.error };
  }

  private async load() {
    try {
      const record = parseChat(native ? await bridge.chatLoad(this.projectId) : null, (message, values) =>
        this.displayText(message, values),
      );
      this.messages = record.messages;
      this.brief = record.brief;
    } catch (e) {
      this.setAppError("无法恢复对话：{error}", { error: errorMessage(e) });
    }
    try {
      if (native) this.permissions = { ...defaultPermissions, ...(await bridge.agentPermissions(this.projectId)) };
    } catch (e) {
      if (!this.error) this.setAppError("无法读取助手权限：{error}", { error: errorMessage(e) });
    }
    if (this.disposed) return;
    this.loaded = true;
    this.notify();
  }

  /** Attach the on-screen view; returns a detach function. Detaching never stops a run. */
  attach(view: SessionView) {
    this.view = view;
    this.lastConfirm = view.confirm;
    this.lastTranslate = view.translate;
    return () => {
      if (this.view !== view) return;
      this.lastDevice = view.device();
      this.view = undefined;
    };
  }

  /** Load history first, then connect the built-in docs MCP before exposing an agent to the view. */
  ensureAgent(providers: () => Provider[], ref: string | undefined, thinkingLevel: Effort) {
    if (this.agent || !this.loaded || this.disposed) return;
    // React may ask again while MCP is initializing. Never create competing agents/subscriptions.
    this.creatingAgent ??= this.initializeAgent(providers, ref, thinkingLevel).catch((error) => {
      if (!this.disposed) {
        this.setAppError("无法连接内置 GizOS 文档服务：{error}", { error: errorMessage(error) });
        this.notify();
      }
      return undefined;
    });
  }

  private async initializeAgent(providers: () => Provider[], ref: string | undefined, thinkingLevel: Effort) {
    const host: AgentHost = {
      projectId: this.projectId,
      onFilesChanged: (path) => this.view?.onFilesChanged(path),
      device: () => this.view?.device() ?? this.lastDevice,
      confirm: (title, body, labels) =>
        (this.view?.confirm ?? this.lastConfirm ?? (async () => false))(title, body, labels),
      setBrief: (value) => this.setBrief(value),
      permissions: () => this.permissions,
      supportsImages: () => this.agent?.state.model.input.includes("image") ?? false,
      translate: (message, values) => this.displayText(message, values),
    };
    const agent = await createAgent(host, providers(), ref, this.messages, {
      thinkingLevel,
      getProviders: providers,
      brief: this.brief,
    });
    if (!agent || this.disposed) return undefined;
    this.agent = agent;
    this.unsubscribe = agent.subscribe(async (event) => {
      if (this.disposed) return;
      if (event.type === "agent_start" || event.type === "turn_start") this.phase = "waiting";
      if (event.type === "message_update") {
        const kind = event.assistantMessageEvent.type;
        if (kind.startsWith("thinking")) this.phase = "thinking";
        else if (kind.startsWith("text")) this.phase = "responding";
      }
      if (event.type === "message_end") {
        if (event.message.role === "assistant")
          for (const block of event.message.content) if (block.type === "toolCall") this.currentTools.add(block.id);
        // Save progress as it happens: the prompt, each reply and each tool result.
        void this.save();
      }
      if (event.type === "tool_execution_start") this.phase = "tools";
      if (event.type === "agent_end") {
        this.phase = "saving";
        this.notify();
        await this.save();
      }
      this.notify();
    });
    this.notify();
    return agent;
  }

  /** Queue an ordered write of the current conversation and brief. */
  save() {
    if (!native || this.disposed) return this.saves;
    const snapshot = serializeChat(this.brief, this.agent?.state.messages ?? this.messages);
    this.saves = this.saves
      .catch(() => {})
      .then(() => bridge.chatSave(this.projectId, snapshot))
      .catch((e) => {
        this.error = errorMessage(e);
        this.notify();
        throw e;
      });
    return this.saves;
  }

  setBrief(value: string) {
    if (value === this.brief) return;
    this.brief = value;
    if (this.agent) updateAgentPrompt(this.agent, systemPromptWith(value, this.permissions));
    this.notify();
    void this.save().catch(() => {});
  }

  /** Saved natively first; the native layer enforces files and web, the tool applies run. */
  async setPermissions(next: AgentPermissions) {
    const previous = this.permissions;
    this.permissions = next;
    // Invalidate queued assistant requests synchronously, then deliver cancellation to their windows.
    const cancellation =
      next.run === "deny" || (previous.run === "allow" && next.run === "ask")
        ? cancelAgentSimulatorRuns(this.projectId, next.run === "ask").then(
            () => undefined,
            (error: unknown) => error,
          )
        : Promise.resolve(undefined);
    if (this.agent) updateAgentPrompt(this.agent, systemPromptWith(this.brief, next));
    this.notify();
    try {
      if (native) await bridge.setAgentPermissions(this.projectId, next);
    } catch (e) {
      this.permissions = previous;
      if (this.agent) updateAgentPrompt(this.agent, systemPromptWith(this.brief, previous));
      this.setAppError("无法保存助手权限：{error}", { error: errorMessage(e) });
      this.notify();
    }
    const cancellationError = await cancellation;
    if (cancellationError !== undefined) {
      this.setAppError("无法停止模拟器请求：{error}", { error: errorMessage(cancellationError) });
      this.notify();
    }
  }

  setError(message: string) {
    this.error = message;
    this.appError = undefined;
    this.notify();
  }

  setDraft(draft: Draft) {
    this.draft = draft;
  }

  async send(text: string, attachments: Attachment[], thinkingLevel: Effort) {
    const agent = this.agent;
    if (!agent || this.running || agent.state.isStreaming || (!text && !attachments.length)) return;
    if (attachments.length && !agent.state.model.input.includes("image")) {
      this.setAppError("当前模型不支持图片输入，换一个支持图片的模型，或移除图片后再发送。");
      this.notify();
      return;
    }
    const restore = () => {
      if (!this.draft.text && !this.draft.attachments.length) {
        this.draft = { text, attachments };
        this.restored++;
      }
    };
    this.currentTools.clear();
    applyThinking(agent, thinkingLevel);
    this.running = true;
    this.stopping = false;
    this.phase = "waiting";
    this.error = "";
    this.draft = { text: "", attachments: [] };
    this.notify();
    try {
      await agent.prompt(
        text,
        attachments.length
          ? attachments.map(({ data, mimeType }) => ({ type: "image" as const, data, mimeType }))
          : undefined,
      );
      const last = agent.state.messages.at(-1) as { stopReason?: string } | undefined;
      if (last?.stopReason === "error") restore();
    } catch (e) {
      if (this.disposed) return;
      this.error = errorMessage(e);
      restore();
    } finally {
      if (!this.disposed && this.agent === agent) {
        // agent_end listeners run BEFORE the SDK clears isStreaming; there is no later idle event.
        this.running = false;
        this.stopping = false;
        this.phase = "idle";
        this.notify();
      }
    }
  }

  stop() {
    if (!this.agent || this.stopping || !this.running) return;
    this.stopping = true;
    this.phase = "stopping";
    this.notify();
    this.agent.abort();
  }

  /**
   * Drop the user message at `index` and everything after it, putting its text
   * and images back in the draft. Files the assistant already changed stay changed.
   */
  rewind(index: number): Draft | undefined {
    const agent = this.agent;
    if (!agent || this.running || agent.state.isStreaming) return undefined;
    const message = agent.state.messages[index] as { role?: string; content?: unknown } | undefined;
    if (message?.role !== "user") return undefined;
    const blocks = Array.isArray(message.content)
      ? (message.content as { type?: string; text?: string; data?: string; mimeType?: string }[])
      : [{ type: "text", text: typeof message.content === "string" ? message.content : "" }];
    const draft: Draft = {
      text: blocks
        .filter((b) => b.type === "text")
        .map((b) => b.text ?? "")
        .join(""),
      attachments: blocks
        .filter((b) => b.type === "image" && typeof b.data === "string" && typeof b.mimeType === "string")
        .map((b, i) => ({
          id: crypto.randomUUID(),
          name: `图片 ${i + 1}`,
          ...(this.view?.translate || this.lastTranslate
            ? { nameKey: "图片 {count}", nameValues: { count: i + 1 } }
            : {}),
          mimeType: b.mimeType!,
          data: b.data!,
        })),
    };
    agent.state.messages = agent.state.messages.slice(0, index);
    updateAgentPrompt(agent, systemPromptWith(this.brief, this.permissions));
    this.currentTools.clear();
    this.error = "";
    this.draft = draft;
    this.restored++;
    this.notify();
    void this.save().catch(() => {});
    return draft;
  }

  /** Ask again from the user message at `index`, replacing the reply after it. */
  async retry(index: number, thinkingLevel: Effort) {
    const draft = this.rewind(index);
    if (!draft) return;
    this.draft = { text: "", attachments: [] };
    this.restored++;
    await this.send(draft.text, draft.attachments, thinkingLevel);
  }

  /** Empty the conversation and keep the brief. */
  async clear() {
    const agent = this.agent;
    if (!agent || this.running || agent.state.isStreaming) return;
    agent.reset();
    this.messages = [];
    this.error = "";
    this.notify();
    await this.save().catch(() => {});
  }

  /** Stop and forget the session, e.g. when its project is deleted. */
  dispose() {
    this.disposed = true;
    this.unsubscribe();
    this.agent?.abort();
    this.listeners.clear();
  }

  /** Before an app update: refuse while running, then flush the latest state. */
  async prepareForUpdate() {
    await this.loading;
    if (this.running || this.agent?.state.isStreaming)
      throw new Error(this.displayText("助手正在生成，请停止或等待完成后安装更新。"));
    if (this.agent) await this.save();
  }
}

const sessions = new Map<string, ProjectSession>();
let updateHook: (() => void) | undefined;

export function projectSession(projectId: string) {
  let session = sessions.get(projectId);
  if (!session) {
    session = new ProjectSession(projectId);
    sessions.set(projectId, session);
  }
  updateHook ??= beforeAppUpdate(async () => {
    for (const item of sessions.values()) await item.prepareForUpdate();
  });
  return session;
}

export function disposeProjectSession(projectId: string) {
  sessions.get(projectId)?.dispose();
  sessions.delete(projectId);
}
