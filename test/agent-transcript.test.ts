import test from "node:test";
import assert from "node:assert/strict";
import { Agent } from "@earendil-works/pi-agent-core";
import { getCurrentTools } from "@earendil-works/pi-ai";
import { obj, str } from "@earendil-works/pi-schema";
import { agentPromptMessage, updateAgentPrompt } from "../src/agent-transcript.ts";

function agent() {
  return new Agent({
    initialState: { messages: [agentPromptMessage("original instructions; run allowed")] },
    streamFn: () => {
      throw new Error("No model calls in transcript tests");
    },
  });
}

test("pi 1.x prompt updates replace named instructions while retaining the conversation", () => {
  const current = agent();
  current.state.messages.push({ role: "user", content: "Keep this project conversation", timestamp: 1 });
  updateAgentPrompt(current, "updated brief; run denied");
  updateAgentPrompt(current, "latest brief; run asks");
  assert.equal(current.state.systemPrompt, "latest brief; run asks");
  assert.equal(current.state.messages.filter((message) => message.role === "user").length, 1);
  assert.equal(current.state.messages.length, 3, "successive prompt patches share the trailing system message");
});

test("prompt patches preserve tool declarations and reset keeps the latest instructions", () => {
  const current = agent();
  current.state.messages.push({
    role: "system",
    content: "",
    timestamp: 2,
    toolsAdded: [{ name: "read_file", description: "Read the project", parameters: obj({ path: str() }) }],
  });
  updateAgentPrompt(current, "new permission state");
  assert.deepEqual(
    getCurrentTools(current.state.messages).map((tool) => tool.name),
    ["read_file"],
  );
  current.reset();
  assert.equal(current.state.systemPrompt, "new permission state");
  assert.deepEqual(
    getCurrentTools(current.state.messages).map((tool) => tool.name),
    ["read_file"],
  );
});
