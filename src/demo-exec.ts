import { execActive, parseExecRequest, type ExecEnvelope, type ExecJob, type ExecResponse } from "./exec.ts";
import { demoSerial } from "./demo-serial.ts";

const storageKey = "gizmoth.demo.exec.jobs.v1";
let memory: ExecJob[] = [];

function history(): ExecJob[] {
  if (typeof localStorage === "undefined") return structuredClone(memory);
  const raw = localStorage.getItem(storageKey);
  if (!raw) return [];
  const value: unknown = JSON.parse(raw);
  if (!Array.isArray(value)) throw new Error("演示任务历史无法读取。请检查浏览器存储。");
  return value as ExecJob[];
}
function save(jobs: ExecJob[]) {
  // Persist only receipts, never execution results, stdout or logs.
  const accepted = jobs.map((job) => ({
    id: job.id,
    name: job.name,
    operation: job.operation,
    request: job.request,
    status: job.status,
    createdAt: job.createdAt,
    startedAt: job.startedAt,
    finishedAt: job.finishedAt,
    error: job.error,
    output: job.output,
    sourceJobId: job.sourceJobId ?? null,
  }));
  if (typeof localStorage !== "undefined") localStorage.setItem(storageKey, JSON.stringify(accepted));
  memory = structuredClone(accepted);
}
function advance(): ExecJob[] {
  const jobs = history();
  const now = Date.now();
  let changed = false;
  for (const job of jobs) {
    if (!execActive(job)) continue;
    const elapsed = now - job.createdAt;
    if (elapsed >= 1800) {
      job.startedAt ??= job.createdAt + 300;
      job.finishedAt = job.createdAt + 1800;
      const command = job.request.spec.command;
      const failed = job.operation === "compile" && Array.isArray(command) && command[0] === "false";
      job.status = failed ? "failed" : "succeeded";
      job.error = failed ? "Demo: command exited with status 1." : null;
      changed = true;
    } else if (elapsed >= 300 && job.status === "queued") {
      job.status = "running";
      job.startedAt = job.createdAt + 300;
      changed = true;
    }
  }
  if (changed) save(jobs);
  return jobs;
}
function find(jobs: ExecJob[], id: string) {
  const job = jobs.find((item) => item.id === id);
  if (!job) throw new Error("未找到这个执行任务。");
  return job;
}
function accept(jobs: ExecJob[], envelope: ExecEnvelope, output: string | null): ExecJob {
  const job: ExecJob = {
    id: `exec-demo-${crypto.randomUUID()}`,
    name: envelope.metadata?.name || envelope.spec.operation,
    operation: envelope.spec.operation,
    request: structuredClone(envelope),
    status: "queued",
    createdAt: Date.now(),
    startedAt: null,
    finishedAt: null,
    error: null,
    output,
  };
  jobs.unshift(job);
  return job;
}
function cancel(jobs: ExecJob[], id: string, request?: ExecEnvelope, output: string | null = null): ExecJob {
  const target = find(jobs, id);
  if (execActive(target)) {
    target.status = "cancelled";
    target.finishedAt = Date.now();
  }
  const receipt = accept(
    jobs,
    request ?? {
      apiVersion: "gizmoth.dev/v1",
      kind: "Request",
      spec: { operation: "jobs.cancel", id },
    },
    output,
  );
  receipt.status = "succeeded";
  receipt.startedAt = receipt.createdAt;
  receipt.finishedAt = receipt.createdAt;
  save(jobs);
  return target;
}

const capabilities = {
  demo: true,
  apiVersion: "gizmoth.dev/v1",
  kind: "Request",
  operations: [
    { operation: "describe", required: [] },
    { operation: "serial.list", required: [] },
    { operation: "serial.ports", required: [] },
    { operation: "serial.status", required: ["port"] },
    {
      operation: "compile",
      required: ["workspace", "command"],
      example: { workspace: "/demo/workspace", command: ["make"] },
    },
    { operation: "jobs.list", required: [] },
    { operation: "jobs.get", required: ["id"] },
    { operation: "jobs.wait", required: ["id"], waitSeconds: "0–120" },
    { operation: "jobs.cancel", required: ["id"] },
    { operation: "logs.read", required: ["source", "id"] },
    { operation: "serial.send", required: ["port", "text"] },
    { operation: "serial.install", required: ["port"], oneOf: ["package", "artifact"] },
    { operation: "serial.refresh", required: ["port"] },
    { operation: "serial.reboot", required: ["port", "target"] },
  ],
};

export function demoExec(command: string, args: Record<string, unknown> = {}): unknown {
  if (command !== "exec_request") throw new Error(`Unknown demo execution command: ${command}`);
  const method = String(args.method ?? "GET");
  const path = String(args.path ?? "").split("?")[0];
  const jobs = advance();
  if (method === "GET" && path === "/v1/exec/jobs") return structuredClone(jobs);
  const match = /^\/v1\/exec\/jobs\/([^/]+)(?:\/(cancel|reveal))?$/.exec(path);
  if (match) {
    const id = decodeURIComponent(match[1]);
    const job = find(jobs, id);
    if (method === "GET" && !match[2]) return { job: structuredClone(job) } satisfies ExecResponse;
    if (method === "POST" && match[2] === "cancel") return { job: structuredClone(cancel(jobs, id)) };
    if (method === "POST" && match[2] === "reveal") throw new Error("演示模式不会创建输出文件。");
  }
  if (method === "POST" && path === "/v1/exec") {
    const body = args.body as { request?: unknown; output?: unknown } | undefined;
    const envelope = parseExecRequest(JSON.stringify(body?.request) ?? "");
    const output = typeof body?.output === "string" ? body.output.trim() || null : null;
    if (output?.includes("\0")) throw new Error("输出文件路径不能包含空字符。");
    const { operation } = envelope.spec;
    if (operation === "describe") return { result: structuredClone(capabilities) };
    if (["serial.list", "serial.ports", "serial.status"].includes(operation)) {
      const allowed = operation === "serial.status" ? ["operation", "port"] : ["operation"];
      if (Object.keys(envelope.spec).some((key) => !allowed.includes(key))) throw new Error("Unknown serial query parameter.");
      if (operation === "serial.ports")
        return { result: { service: "running", ports: demoSerial("serial_system_ports", {}) } };
      const { ports } = demoSerial("serial_overview", {}) as { ports: Record<string, unknown>[] };
      if (operation === "serial.list") return { result: { service: "running", ports } };
      if (typeof envelope.spec.port !== "string" || !/^[A-Za-z0-9-]{1,128}$/.test(envelope.spec.port))
        throw new Error("Invalid port");
      const port = ports.find((port) => port.id === envelope.spec.port);
      if (!port) throw new Error("Serial port not found.");
      return { result: port };
    }
    if (operation === "jobs.list") return { result: structuredClone(jobs) };
    if (operation === "jobs.get" || operation === "jobs.wait") {
      if (typeof envelope.spec.id !== "string") throw new Error("任务 ID 无效。");
      if (
        envelope.spec.waitSeconds !== undefined &&
        (!Number.isInteger(envelope.spec.waitSeconds) ||
          Number(envelope.spec.waitSeconds) < 0 ||
          Number(envelope.spec.waitSeconds) > 120)
      )
        throw new Error("waitSeconds must be between 0 and 120.");
      return { result: { job: structuredClone(find(jobs, envelope.spec.id)) } };
    }
    if (operation === "logs.read") {
      if (!["compile", "serial"].includes(String(envelope.spec.source)) || typeof envelope.spec.id !== "string")
        throw new Error("logs.read requires source and id.");
      return { result: { entries: [], next: 0, demo: true } };
    }
    if (operation === "jobs.cancel") {
      if (typeof envelope.spec.id !== "string") throw new Error("任务 ID 无效。");
      const target = cancel(jobs, envelope.spec.id, envelope, output);
      return { job: structuredClone(jobs[0]), result: { job: structuredClone(target) } };
    }
    if (operation === "compile") {
      if (
        typeof envelope.spec.workspace !== "string" ||
        !envelope.spec.workspace.trim() ||
        !Array.isArray(envelope.spec.command) ||
        !envelope.spec.command.length ||
        !envelope.spec.command.every((part) => typeof part === "string")
      )
        throw new Error("compile requires workspace and a command array.");
    } else if (["serial.send", "serial.install", "serial.refresh", "serial.reboot"].includes(operation)) {
      if (typeof envelope.spec.port !== "string" || !envelope.spec.port.trim())
        throw new Error("A serial operation requires port.");
    } else throw new Error(`Unsupported operation: ${operation}. Run describe to discover operations.`);
    const job = accept(jobs, envelope, output);
    save(jobs);
    return { job: structuredClone(job) } satisfies ExecResponse;
  }
  throw new Error(`Unknown demo execution endpoint: ${method} ${path}`);
}
