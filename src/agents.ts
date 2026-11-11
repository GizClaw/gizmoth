import { call } from "./bridge";

export interface AgentTarget {
  id: string;
  name: string;
  path: string;
  installed: boolean;
  current: boolean;
  error?: string | null;
}
export interface AgentSetup {
  skill: string;
  targets: AgentTarget[];
}

export const agents = {
  status: () => call<AgentSetup>("agents_status"),
  set: (id: string, installed: boolean) => call<AgentSetup>("agents_set", { id, installed }),
};
