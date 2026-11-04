import type { AgentMessage } from "@mariozechner/pi-agent-core";
import type { Api, Model } from "@mariozechner/pi-ai";
import type { ChatCheckpoint, ChatEntry, ChatPage, ChatState } from "./bridge";

export interface ChatStorage {
  page(options: { after?: number; before?: number; limit?: number }): Promise<ChatPage>;
  append(epoch: string, expected: number, messages: AgentMessage[]): Promise<ChatState>;
  checkpoint(epoch: string, value: ChatCheckpoint): Promise<ChatState>;
}
export interface ContextBudget {
  input: number;
  summary: number;
  chunk: number;
  message: number;
}
export type Summarize = (
  previous: string,
  transcript: string,
  maxTokens: number,
  signal?: AbortSignal,
) => Promise<string>;

/** Conservative estimate, including Chinese text and JSON/tool-call overhead. */
export const estimateTokens = (value: unknown) =>
  Math.ceil(new TextEncoder().encode(typeof value === "string" ? value : JSON.stringify(value)).length / 3) + 12;

export function contextBudget(model: Pick<Model<Api>, "contextWindow" | "maxTokens">, overhead: number): ContextBudget {
  // pi-ai's streamSimple requests min(model.maxTokens, 32000) by default.
  const output = Math.min(model.maxTokens, 32_000);
  const available = model.contextWindow - output - overhead - 512;
  if (available < 512) throw new Error("当前模型的上下文窗口太小，请选择更大的模型。");
  const input = Math.min(48_000, Math.floor(available * 0.75));
  const summary = Math.min(2048, Math.max(96, Math.floor(input * 0.15)));
  return {
    input,
    summary,
    chunk: Math.max(128, Math.min(16_000, input - summary - 256)),
    message: Math.max(128, Math.min(3000, Math.floor(input * 0.25))),
  };
}

function clipped(text: string, limit: number) {
  if (estimateTokens(text) <= limit) return text;
  // Only a copy sent to the model is shortened; the JSONL record stays intact.
  const chars = Math.max(48, Math.floor(limit * 0.8));
  return `${text.slice(0, chars)}\n[Historical content omitted from model context; complete content remains in conversation history. Read the project file again when exact contents are needed.]\n${text.slice(-Math.floor(chars / 4))}`;
}

function briefArguments(value: unknown, limit: number): unknown {
  if (typeof value === "string") return clipped(value, limit);
  if (Array.isArray(value))
    return value.slice(0, 32).map((v) => briefArguments(v, Math.max(24, Math.floor(limit / value.length))));
  if (value && typeof value === "object") {
    const entries = Object.entries(value);
    if (entries.length > 64) return { omitted: "Historical arguments are in the complete conversation history." };
    return Object.fromEntries(
      entries.map(([k, v]) => [k, briefArguments(v, Math.max(24, Math.floor(limit / Math.max(entries.length, 1))))]),
    );
  }
  return value;
}

/** Keep role/tool-call IDs and ordering intact while bounding large historical payloads. */
export function projectMessage(message: AgentMessage, limit: number): AgentMessage {
  const copy = { ...message } as Record<string, unknown>;
  delete copy.details;
  if (typeof copy.content === "string") copy.content = clipped(copy.content, limit);
  else if (Array.isArray(copy.content)) {
    const perBlock = Math.max(32, Math.floor(limit / Math.max(copy.content.length, 1)));
    copy.content = copy.content.map((raw: Record<string, unknown>) => {
      const block = { ...raw };
      if (typeof block.text === "string") block.text = clipped(block.text, perBlock);
      if (typeof block.thinking === "string") block.thinking = clipped(block.thinking, perBlock);
      if (block.type === "toolCall") block.arguments = briefArguments(block.arguments, perBlock);
      if (block.type === "image")
        return { type: "text", text: "[Historical image is available in the complete conversation history.]" };
      return block;
    });
  }
  return copy as unknown as AgentMessage;
}

function projected(entries: ChatEntry[], budget: ContextBudget, keepLatestUser: boolean) {
  const intact = entries.map(({ message }) => {
    const copy = { ...message } as Record<string, unknown>;
    delete copy.details;
    return copy as unknown as AgentMessage;
  });
  // Keep recent file contents and tool results intact whenever the context fits.
  // Projection is a last resort for an oversized single turn, not routine truncation.
  if (estimateTokens(intact) + budget.summary + 96 <= budget.input) return intact;
  let latestUser = -1;
  if (keepLatestUser)
    for (let i = entries.length - 1; i >= 0; i--)
      if (entries[i].message.role === "user") {
        latestUser = i;
        break;
      }
  return entries.map((entry, i) => (i === latestUser ? entry.message : projectMessage(entry.message, budget.message)));
}

/** The checkpoint ends just before a user message, never inside a tool exchange. */
export function compactionCut(entries: ChatEntry[], budget: ContextBudget): number {
  const starts = entries.flatMap((entry, i) => (entry.message.role === "user" ? [i] : []));
  for (const keep of [2, 1]) {
    const cut = starts[starts.length - keep];
    if (cut > 0 && (keep === 1 || estimateTokens(projected(entries.slice(cut), budget, true)) < budget.input * 0.65))
      return cut;
  }
  return 0;
}

const checkAbort = (signal?: AbortSignal) => signal?.throwIfAborted();

/** Owns only the unsummarized tail. Complete history belongs to the append-only store. */
export class ChatSession {
  state: ChatState;
  private entries: ChatEntry[] = [];
  private restored = false;
  private seen = new WeakSet<object>();
  private queued = new WeakSet<object>();
  private pending: AgentMessage[] = [];
  private inFlight?: { start: number; messages: AgentMessage[] };

  private storage: ChatStorage;
  private onAdded: (entry: ChatEntry) => void;
  private onState: (state: ChatState) => void;

  constructor(
    storage: ChatStorage,
    initial: ChatState,
    onAdded: (entry: ChatEntry) => void = () => {},
    onState: (state: ChatState) => void = () => {},
  ) {
    this.storage = storage;
    this.state = initial;
    this.onAdded = onAdded;
    this.onState = onState;
  }

  async record(messages: AgentMessage[]) {
    for (const message of messages) {
      if (
        !["user", "assistant", "toolResult"].includes(message.role) ||
        this.seen.has(message) ||
        this.queued.has(message)
      )
        continue;
      this.pending.push(message);
      this.queued.add(message);
    }
    await this.flush();
  }

  async flush() {
    while (this.pending.length) {
      // Retry the exact uncertain batch before adding any later failure message.
      // An append may have reached disk even if its IPC acknowledgement was lost.
      this.inFlight ??= { start: this.state.total, messages: this.pending.slice() };
      const { start, messages: batch } = this.inFlight;
      const result = await this.storage.append(this.state.epoch, start, batch);
      if (result.epoch !== this.state.epoch || result.total !== start + batch.length)
        throw new Error("对话已在其他窗口更新，请重新打开项目。记录不会被覆盖。");
      this.state = result;
      this.pending.splice(0, batch.length);
      this.inFlight = undefined;
      for (const [i, message] of batch.entries()) {
        this.seen.add(message);
        this.queued.delete(message);
        const entry = { seq: start + i + 1, message };
        if (this.restored) this.entries.push(entry);
        this.onAdded(entry);
      }
      this.onState(result);
    }
  }

  private async compact(budget: ContextBudget, summarize: Summarize, signal?: AbortSignal) {
    const previous = this.state.checkpoint?.summary ?? "";
    if (estimateTokens(previous) + estimateTokens(this.entries) <= budget.input) return;
    const cut = compactionCut(this.entries, budget);
    if (!cut) return;
    const prefix = this.entries.slice(0, cut);
    let summary = previous;
    let chunk: AgentMessage[] = [];
    const apply = async () => {
      if (!chunk.length) return;
      checkAbort(signal);
      const next = (await summarize(summary, JSON.stringify(chunk), budget.summary, signal)).trim();
      checkAbort(signal);
      if (!next || estimateTokens(next) > budget.summary * 1.5)
        throw new Error("上下文摘要为空或超过预算，原始记录已保留，请重试或切换模型。");
      summary = next;
      chunk = [];
    };
    for (const entry of prefix) {
      const message = projectMessage(entry.message, Math.max(96, Math.floor(budget.chunk / 2)));
      if (chunk.length && estimateTokens([...chunk, message]) > budget.chunk) await apply();
      chunk.push(message);
    }
    await apply();
    checkAbort(signal);
    const next = await this.storage.checkpoint(this.state.epoch, { through: prefix.at(-1)!.seq, summary });
    if (next.epoch !== this.state.epoch || next.total !== this.state.total)
      throw new Error("对话已更新，请重新打开项目后继续。");
    this.state = next;
    this.entries = this.entries.slice(cut);
    this.onState(next);
  }

  async forModel(budget: ContextBudget, summarize: Summarize, signal?: AbortSignal): Promise<AgentMessage[]> {
    await this.flush();
    checkAbort(signal);
    if (!this.restored) {
      this.entries = [];
      let after = this.state.checkpoint?.through ?? 0;
      while (after < this.state.total) {
        checkAbort(signal);
        const page = await this.storage.page({ after, limit: 40 });
        if (
          page.epoch !== this.state.epoch ||
          page.total !== this.state.total ||
          !page.entries.length ||
          page.entries[0].seq !== after + 1
        )
          throw new Error("对话记录已变化或不完整，请重新打开项目。");
        this.entries.push(...page.entries);
        after = page.entries.at(-1)!.seq;
        await this.compact(budget, summarize, signal);
      }
      this.restored = true;
    }
    await this.compact(budget, summarize, signal);
    checkAbort(signal);
    const messages = projected(
      this.entries.filter((e) => e.seq > (this.state.checkpoint?.through ?? 0)),
      budget,
      true,
    );
    const summary = this.state.checkpoint?.summary;
    if (summary)
      messages.unshift({
        role: "user",
        content: `[Earlier project conversation summary. Use this as background; the user's newest message follows.]\n${summary}`,
        timestamp: 0,
      });
    if (estimateTokens(messages) > budget.input)
      throw new Error(
        "当前这一轮对话仍超过模型上下文预算。请换用更大窗口的模型，或用一条简短消息开始下一轮；完整历史已保留。",
      );
    return messages;
  }
}
