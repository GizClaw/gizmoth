import type { Agent } from "@earendil-works/pi-agent-core";
import type { SystemMessage } from "@earendil-works/pi-ai";

/** A named section lets brief/permission updates replace instructions rather than append stale copies. */
export function agentPromptMessage(prompt: string): SystemMessage {
  return { role: "system", content: "", sections: { gizmoth: prompt }, timestamp: Date.now() };
}

export function updateAgentPrompt(agent: Pick<Agent, "state">, prompt: string) {
  const messages = agent.state.messages;
  const last = messages.at(-1);
  if (last?.role === "system") {
    const previous = last as SystemMessage;
    agent.state.messages = [
      ...messages.slice(0, -1),
      {
        ...previous,
        sections: { ...previous.sections, gizmoth: prompt },
        timestamp: Date.now(),
      },
    ];
  } else agent.state.messages = [...messages, agentPromptMessage(prompt)];
}
