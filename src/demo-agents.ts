import type { AgentSetup } from "./agents";
import skillFixture from "./demo-agent-skill.json" with { type: "json" };

// Exact native template with example paths. No user files, services or credentials are accessed.
const setup: AgentSetup = {
  skill: skillFixture.skill,
  targets: [
    {
      id: "claude-code",
      name: "Claude Code",
      path: "/Users/example/.claude/skills/gizmoth/SKILL.md",
      installed: false,
      current: false,
    },
    {
      id: "codex",
      name: "Codex",
      path: "/Users/example/.agents/skills/gizmoth/SKILL.md",
      installed: false,
      current: false,
    },
    {
      id: "gemini-cli",
      name: "Gemini CLI",
      path: "/Users/example/.gemini/skills/gizmoth/SKILL.md",
      installed: false,
      current: false,
    },
    {
      id: "opencode",
      name: "OpenCode",
      path: "/Users/example/.config/opencode/skills/gizmoth/SKILL.md",
      installed: false,
      current: false,
    },
    {
      id: "pi",
      name: "Pi",
      path: "/Users/example/.pi/agent/skills/gizmoth/SKILL.md",
      installed: false,
      current: false,
    },
  ],
};
export function demoAgents(command: string, args: Record<string, unknown>): AgentSetup {
  if (command === "agents_set") {
    const target = setup.targets.find((item) => item.id === args.id);
    if (!target) throw new Error("Unknown agent");
    target.installed = Boolean(args.installed);
    target.current = target.installed;
  } else if (command !== "agents_status") throw new Error(`Unknown agent command: ${command}`);
  return structuredClone(setup);
}
