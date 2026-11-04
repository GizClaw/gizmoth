import assert from "node:assert/strict";
import test from "node:test";
import { Agent, type AgentMessage } from "@mariozechner/pi-agent-core";
import { createAssistantMessageEventStream, type AssistantMessage, type Model } from "@mariozechner/pi-ai";
import type { ChatCheckpoint, ChatEntry, ChatPage, ChatState } from "../src/bridge.ts";
import {
  ChatSession,
  compactionCut,
  contextBudget,
  estimateTokens,
  projectMessage,
  type ChatStorage,
  type ContextBudget,
} from "../src/chat-context.ts";

const user = (text: string): AgentMessage => ({ role: "user", content: text, timestamp: 1 });
const assistant = (text: string): AgentMessage =>
  ({ role: "assistant", content: [{ type: "text", text }], stopReason: "stop", timestamp: 2 }) as AgentMessage;
const budget: ContextBudget = { input: 2200, summary: 250, chunk: 1200, message: 400 };

class MemoryChat implements ChatStorage {
  records: ChatEntry[];
  state: ChatState;
  reads = 0;
  checkpoints = 0;
  loseAck = false;
  constructor(messages: AgentMessage[] = [], checkpoint: ChatCheckpoint | null = null) {
    this.records = messages.map((message, i) => ({ seq: i + 1, message }));
    this.state = { epoch: "test-epoch", total: messages.length, checkpoint };
  }
  async page(options: { after?: number; before?: number; limit?: number }): Promise<ChatPage> {
    this.reads++;
    const limit = options.limit ?? 40;
    const entries =
      options.after === undefined
        ? this.records.filter((e) => options.before === undefined || e.seq < options.before).slice(-limit)
        : this.records.filter((e) => e.seq > options.after!).slice(0, limit);
    return { epoch: this.state.epoch, total: this.state.total, entries: structuredClone(entries) };
  }
  async append(epoch: string, expected: number, messages: AgentMessage[]): Promise<ChatState> {
    assert.equal(epoch, this.state.epoch);
    if (expected < this.state.total) {
      assert.deepEqual(
        this.records.slice(expected, expected + messages.length).map((e) => e.message),
        messages,
      );
      return structuredClone(this.state);
    }
    assert.equal(expected, this.state.total);
    this.records.push(...messages.map((message, i) => ({ seq: expected + i + 1, message: structuredClone(message) })));
    this.state.total += messages.length;
    if (this.loseAck) {
      this.loseAck = false;
      throw new Error("lost acknowledgement");
    }
    return structuredClone(this.state);
  }
  async checkpoint(epoch: string, value: ChatCheckpoint): Promise<ChatState> {
    assert.equal(epoch, this.state.epoch);
    assert.ok(value.through <= this.state.total);
    if (value.through < this.state.total) assert.equal(this.records[value.through].message.role, "user");
    this.state.checkpoint = structuredClone(value);
    this.checkpoints++;
    return structuredClone(this.state);
  }
}

test("appends each completed message once and retries a lost acknowledgement without duplication", async () => {
  const store = new MemoryChat();
  const rendered: ChatEntry[] = [];
  const session = new ChatSession(store, structuredClone(store.state), (e) => rendered.push(e));
  const message = user("keep this goal");
  store.loseAck = true;
  await assert.rejects(session.record([message]), /acknowledgement/);
  await session.record([message]);
  await session.record([message]);
  assert.equal(store.state.total, 1);
  assert.equal(rendered.length, 1);
  const context = await session.forModel(budget, async () => {
    throw new Error("unnecessary summary");
  });
  assert.equal(store.reads, 1);
  assert.deepEqual(context, [message]);
  await session.record([assistant("done")]);
  assert.equal((await session.forModel(budget, async () => "unused")).length, 2);
  assert.equal(store.reads, 1, "live tail needs no history reread");
});

test("restores in pages, persists bounded summaries and keeps the complete history untouched", async () => {
  const messages = Array.from({ length: 90 }, (_, i) => [
    user(`Goal ${i}: ` + "中文要求".repeat(160)),
    assistant(`Done ${i}: ` + "details ".repeat(160)),
  ]).flat();
  const store = new MemoryChat(messages);
  const before = JSON.stringify(store.records);
  const session = new ChatSession(store, structuredClone(store.state));
  assert.equal(store.reads, 0, "constructing/opening history does not load full model context");
  const inputs: string[] = [];
  const context = await session.forModel(budget, async (previous, transcript) => {
    inputs.push(transcript);
    assert.ok(estimateTokens(transcript) <= budget.chunk + 100);
    return "Preserved project goals, filenames and pending work. " + (previous ? "Earlier decisions retained." : "");
  });
  assert.ok(store.reads > 1);
  assert.ok(store.checkpoints > 0);
  assert.ok(inputs.length > 1);
  assert.ok(store.state.checkpoint!.through > 0);
  assert.equal(store.records[store.state.checkpoint!.through].message.role, "user");
  assert.equal(JSON.stringify(store.records), before);
  assert.ok(estimateTokens(context) <= budget.input);
  assert.ok(JSON.stringify(context[0]).includes("Earlier project conversation summary"));
  assert.ok(JSON.stringify(context).includes("Goal 89"));
  const restored = new MemoryChat(messages, store.state.checkpoint);
  const reopened = new ChatSession(restored, structuredClone(restored.state));
  const resumed = await reopened.forModel(budget, async () => {
    throw new Error("should use the saved summary");
  });
  assert.deepEqual(resumed, context);
  assert.equal(restored.reads, 1);
});

test("an uncertain append is retried before persisting the runtime's failure message", async () => {
  const store = new MemoryChat();
  const session = new ChatSession(store, structuredClone(store.state));
  const request = user("keep the newest request");
  store.loseAck = true;
  await assert.rejects(session.record([request]), /acknowledgement/);
  const failure = assistant("save failure");
  await session.record([request, failure]);
  assert.deepEqual(
    store.records.map((entry) => entry.message),
    [request, failure],
  );
  assert.equal(session.state.total, 2);
  await session.flush();
  assert.equal(store.records.length, 2);
});

test("summary failures and cancellation never advance a checkpoint or lose messages", async () => {
  const messages = Array.from({ length: 8 }, (_, i) => [
    user(`Goal ${i}: ` + "x".repeat(2000)),
    assistant("result".repeat(300)),
  ]).flat();
  const store = new MemoryChat(messages);
  const session = new ChatSession(store, structuredClone(store.state));
  await assert.rejects(
    session.forModel(budget, async () => {
      throw new Error("provider failed");
    }),
    /provider failed/,
  );
  assert.equal(store.state.checkpoint, null);
  assert.equal(store.records.length, messages.length);
  const controller = new AbortController();
  await assert.rejects(
    session.forModel(
      budget,
      async () => {
        controller.abort();
        return "summary";
      },
      controller.signal,
    ),
    /abort/i,
  );
  assert.equal(store.state.checkpoint, null);
  await session.forModel(budget, async () => "The project goal remains unchanged.");
  assert.ok(store.state.checkpoint);
});

test("compaction preserves complete tool exchanges and projects large payloads without changing history", () => {
  const call: AgentMessage = {
    ...assistant(""),
    role: "assistant",
    content: [
      {
        type: "toolCall",
        id: "call-1",
        name: "write_file",
        arguments: { path: "app.lua", content: "large-source".repeat(5000) },
      },
    ],
    api: "openai-completions",
    provider: "test",
    model: "test",
    usage: {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
    stopReason: "toolUse",
    timestamp: 1,
  };
  const result = {
    role: "toolResult",
    toolCallId: "call-1",
    toolName: "write_file",
    content: [{ type: "text", text: "large-result".repeat(5000) }],
    isError: false,
    timestamp: 1,
  } as AgentMessage;
  const messages = [user("first"), call, result, assistant("done"), user("second"), assistant("done"), user("third")];
  const entries = messages.map((message, i) => ({ seq: i + 1, message }));
  const cut = compactionCut(entries, budget);
  assert.equal(messages[cut].role, "user");
  assert.ok(cut >= 4);
  const before = JSON.stringify(messages);
  const projected = [projectMessage(call, 400), projectMessage(result, 400)];
  const [projectedCall, projectedResult] = projected;
  assert.equal(projectedCall.role, "assistant");
  assert.ok(projectedCall.role === "assistant");
  const toolCall = projectedCall.content.find((b) => b.type === "toolCall");
  assert.equal(toolCall?.id, "call-1");
  assert.equal(toolCall?.arguments.path, "app.lua");
  assert.ok(projectedResult.role === "toolResult");
  assert.equal(projectedResult.toolCallId, "call-1");
  assert.ok(estimateTokens(projected) < 800);
  assert.equal(JSON.stringify(messages), before);
});

test("recent tool results remain complete when they fit the model input budget", async () => {
  const result: AgentMessage = {
    role: "toolResult",
    toolName: "read_file",
    toolCallId: "call-1",
    content: [{ type: "text", text: "full file contents ".repeat(120) }],
    isError: false,
    timestamp: 1,
  };
  assert.ok(estimateTokens(result) > budget.message);
  const store = new MemoryChat([user("read this file"), result]);
  const session = new ChatSession(store, structuredClone(store.state));
  const context = await session.forModel(budget, async () => {
    throw new Error("no summary needed");
  });
  assert.deepEqual(context.at(-1), result);
});

test("an oversized new user message fails visibly instead of silently truncating the request", async () => {
  const message = user("中文".repeat(5000));
  const store = new MemoryChat([message]);
  const session = new ChatSession(store, structuredClone(store.state));
  await assert.rejects(
    session.forModel(budget, async () => "unused"),
    /超过模型上下文预算/,
  );
  assert.deepEqual(store.records[0].message, message);
  assert.equal(store.state.checkpoint, null);
});

test("pi-agent persists completed messages before tools and restores a bounded context for every request", async () => {
  const store = new MemoryChat(
    Array.from({ length: 12 }, (_, i) => [
      user(`Old goal ${i} ` + "history ".repeat(450)),
      assistant("done ".repeat(200)),
    ]).flat(),
  );
  const session = new ChatSession(store, structuredClone(store.state));
  const model: Model<"openai-completions"> = {
    id: "test",
    name: "Test",
    api: "openai-completions",
    provider: "test",
    baseUrl: "https://example.invalid",
    reasoning: false,
    input: ["text"],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: 8192,
    maxTokens: 2048,
  };
  const usage = {
    input: 0,
    output: 0,
    cacheRead: 0,
    cacheWrite: 0,
    totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
  };
  let requests = 0;
  let tools = 0;
  const before = store.state.total;
  const agent = new Agent({
    initialState: {
      model,
      systemPrompt: "Test coding agent",
      messages: [],
      tools: [
        {
          name: "read_file",
          label: "Read",
          description: "Read synthetic content",
          parameters: { type: "object", properties: {} },
          execute: async () => {
            tools++;
            assert.equal(
              store.records.at(-1)!.message.role,
              "assistant",
              "tool calls must be persisted before execution",
            );
            return { content: [{ type: "text", text: "source content" }], details: {} };
          },
        },
      ],
    },
    getApiKey: () => "synthetic-key",
    transformContext: (_messages, signal) =>
      session.forModel(budget, async () => "Earlier goals and file paths retained.", signal),
    streamFn: (_model, context) => {
      assert.equal(store.records.at(-1)!.message.role, requests ? "toolResult" : "user");
      assert.ok(estimateTokens(context.messages) <= budget.input);
      assert.ok(JSON.stringify(context.messages).includes("Earlier project conversation summary"));
      assert.ok(JSON.stringify(context.messages).includes("Read app.lua"));
      const stream = createAssistantMessageEventStream();
      const message: AssistantMessage = {
        role: "assistant",
        content: requests++
          ? [{ type: "text", text: "Finished reading" }]
          : [{ type: "toolCall", id: "tc-1", name: "read_file", arguments: {} }],
        api: model.api,
        provider: model.provider,
        model: model.id,
        usage,
        stopReason: requests === 1 ? "toolUse" : "stop",
        timestamp: Date.now(),
      };
      stream.push({ type: "done", reason: message.stopReason as "stop" | "toolUse", message });
      return stream;
    },
  });
  agent.subscribe(async (event) => {
    if (event.type === "message_end") await session.record([event.message]);
    if (event.type === "agent_end") await session.record(event.messages);
  });
  await agent.prompt("Read app.lua");
  assert.equal(agent.state.errorMessage, undefined);
  assert.equal(requests, 2);
  assert.equal(tools, 1);
  assert.equal(store.state.total, before + 4);
  assert.equal(store.records.at(-1)!.message.role, "assistant");
});

test("model context budget reserves tool/system/output tokens and accounts for UTF-8", () => {
  const small = contextBudget({ contextWindow: 8192, maxTokens: 4096 }, 1800);
  assert.ok(small.input + 1800 + 4096 < 8192);
  assert.ok(estimateTokens("中文".repeat(1000)) > 1900);
  assert.throws(() => contextBudget({ contextWindow: 1024, maxTokens: 512 }, 800), /太小/);
});
