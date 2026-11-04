import { Agent, type AgentMessage, type AgentTool } from "@mariozechner/pi-agent-core";
import { Type } from "@mariozechner/pi-ai";
import { bridge, type Device, type Provider } from "./bridge";
import { resolveModel, toPiModel } from "./providers";

export interface AgentHost {
  projectId: string;
  /** Called after any file changes so the editor reloads. */
  onFilesChanged: (path?: string) => void;
  device: () => Device | undefined;
  confirm: (title: string, body: string) => Promise<boolean>;
}

export const systemPrompt = `You are GizMoth, a coding assistant embedded in a desktop app for building Lua apps that run on GizOS devices.

Project rules (validated by the device firmware):
- app.json holds {"app_id", "version" (SemVer), "display_name", "description"}. app_id is 1-32 chars of [a-z0-9_.-], not starting with a dot.
- The entry file must be exactly "<app_id>.lua" at the project root and must be Lua source text (no bytecode).
- Every other file must live under "<app_id>/". Paths are relative, no hidden or empty segments. Up to 64 payload files, entry ≤ 256 KiB, payload ≤ 2 MiB, all UTF-8 text.
- The manifest, sizes and SHA-256 hashes are generated automatically when packaging; never write manifest.json.

How to work:
- Inspect the project with list_files/read_file before changing it. Prefer edit_file for small changes and write_file for new files.
- Run check_package after edits and fix every reported problem.
- Use only standard Lua 5.4 libraries unless the user tells you a device API exists. Do not invent device functions; if something needs a device capability you are unsure of, ask.
- Only call push_to_device when the user asks to install or try the app; the user must approve it.
- Keep replies short. Answer in the user's language.`;

const text = (value: string) => [{ type: "text" as const, text: value }];

export function createTools(host: AgentHost): AgentTool[] {
  const id = host.projectId;
  return [
    {
      name: "list_files",
      label: "列出文件",
      description: "List every file in the project with its size in bytes.",
      parameters: Type.Object({}),
      execute: async () => {
        const files = await bridge.files(id);
        return { content: text(files.map((f) => `${f.path}\t${f.size}`).join("\n") || "(empty)"), details: { files } };
      },
    },
    {
      name: "read_file",
      label: "读取文件",
      description: "Read a UTF-8 text file from the project.",
      parameters: Type.Object({ path: Type.String({ description: "Project-relative path" }) }),
      execute: async (_id, { path }) => ({ content: text(await bridge.read(id, path)), details: { path } }),
    },
    {
      name: "write_file",
      label: "写入文件",
      description:
        "Create or overwrite a UTF-8 text file in the project. Parent directories are created automatically.",
      parameters: Type.Object({ path: Type.String(), content: Type.String() }),
      executionMode: "sequential",
      execute: async (_id, { path, content }) => {
        await bridge.write(id, path, content);
        host.onFilesChanged(path);
        return {
          content: text(`Wrote ${path} (${new TextEncoder().encode(content).length} bytes)`),
          details: { path },
        };
      },
    },
    {
      name: "edit_file",
      label: "修改文件",
      description:
        "Replace one exact occurrence of old_text with new_text in a file. old_text must match exactly once.",
      parameters: Type.Object({ path: Type.String(), old_text: Type.String(), new_text: Type.String() }),
      executionMode: "sequential",
      execute: async (_id, { path, old_text, new_text }) => {
        const current = await bridge.read(id, path);
        const first = current.indexOf(old_text);
        if (!old_text || first < 0) throw new Error(`old_text was not found in ${path}`);
        if (current.indexOf(old_text, first + 1) >= 0)
          throw new Error(`old_text occurs more than once in ${path}; include more context`);
        await bridge.write(id, path, current.slice(0, first) + new_text + current.slice(first + old_text.length));
        host.onFilesChanged(path);
        return { content: text(`Edited ${path}`), details: { path } };
      },
    },
    {
      name: "delete_file",
      label: "删除文件",
      description: "Delete a file from the project (app.json cannot be deleted).",
      parameters: Type.Object({ path: Type.String() }),
      executionMode: "sequential",
      execute: async (_id, { path }) => {
        await bridge.removeFile(id, path);
        host.onFilesChanged();
        return { content: text(`Deleted ${path}`), details: { path } };
      },
    },
    {
      name: "check_package",
      label: "检查程序包",
      description:
        "Validate the project and build the device package. Returns the package size or the first problem found.",
      parameters: Type.Object({}),
      execute: async () => {
        const info = await bridge.check(id);
        return {
          content: text(
            `OK: ${info.appId} ${info.version}, ${info.files} files, ${info.bytes} bytes compressed (direct install limit 524288).`,
          ),
          details: info,
        };
      },
    },
    {
      name: "push_to_device",
      label: "推送到设备",
      description: "Install the current project on the selected device and start it. Requires user approval.",
      parameters: Type.Object({}),
      executionMode: "sequential",
      execute: async () => {
        const device = host.device();
        if (!device) throw new Error("No device is selected. Ask the user to pair or select a device.");
        const info = await bridge.check(id);
        const ok = await host.confirm(
          "推送到设备",
          `助手请求把 ${info.appId} ${info.version} 安装到「${device.name}」并运行。同名程序会被替换。`,
        );
        if (!ok) throw new Error("The user declined the install.");
        await bridge.install(id, device.id);
        await bridge.run(device.id, info.appId);
        return {
          content: text(
            `Installed ${info.appId} ${info.version} on ${device.name}; the device accepted the run request.`,
          ),
          details: info,
        };
      },
    },
  ] as AgentTool[];
}

export function createAgent(
  host: AgentHost,
  providers: Provider[],
  modelRef: string | undefined,
  messages: AgentMessage[],
) {
  const resolved = resolveModel(providers, modelRef);
  if (!resolved) return undefined;
  return new Agent({
    initialState: {
      systemPrompt,
      model: toPiModel(resolved.provider, resolved.model),
      thinkingLevel: "off",
      tools: createTools(host),
      messages,
    },
    sessionId: host.projectId,
    toolExecution: "sequential",
    // Keys stay in the OS credential store until a request needs them. Local
    // gateways without auth still need a non-empty placeholder for the SDKs.
    getApiKey: async (providerId) => (await bridge.providerKey(providerId)) ?? "not-required",
  });
}

export function setModel(agent: Agent, providers: Provider[], ref: string) {
  const resolved = resolveModel(providers, ref);
  if (resolved) agent.state.model = toPiModel(resolved.provider, resolved.model);
}

/** Drop transient partial state; keep only completed LLM-visible messages. */
export function storableMessages(messages: AgentMessage[]) {
  return messages.filter((m) => ["user", "assistant", "toolResult"].includes((m as { role: string }).role));
}
