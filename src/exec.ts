export interface ExecEnvelope {
  apiVersion: "gizmoth.dev/v1";
  kind: "Request";
  metadata?: { name?: string; [key: string]: unknown };
  spec: { operation: string; [key: string]: unknown };
  [key: string]: unknown;
}

export type ExecStatus =
  "queued" | "running" | "awaiting_approval" | "succeeded" | "failed" | "cancelled" | "interrupted";

export interface ExecJob {
  id: string;
  name: string;
  operation: string;
  request: ExecEnvelope;
  status: ExecStatus;
  createdAt: number;
  startedAt: number | null;
  finishedAt: number | null;
  error: string | null;
  output: string | null;
  sourceJobId?: string | null;
}

export interface ExecResponse {
  job?: ExecJob;
  result?: unknown;
}

export const initialExecRequest = JSON.stringify(
  {
    apiVersion: "gizmoth.dev/v1",
    kind: "Request",
    metadata: { name: "Discover capabilities" },
    spec: { operation: "describe" },
  },
  null,
  2,
);

const object = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** Validate the shared request envelope; operation-specific validation belongs to the executor. */
export function parseExecRequest(text: string): ExecEnvelope {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error("JSON 格式无效，请检查引号、逗号和括号。");
  }
  if (!object(value)) throw new Error("请求必须是 JSON 对象。");
  if (value.apiVersion !== "gizmoth.dev/v1" || value.kind !== "Request")
    throw new Error('请求必须包含 apiVersion: "gizmoth.dev/v1" 和 kind: "Request"。');
  if (!object(value.spec) || typeof value.spec.operation !== "string" || !value.spec.operation.trim())
    throw new Error("请在 spec.operation 中填写执行操作。");
  if (
    value.metadata !== undefined &&
    (!object(value.metadata) || (value.metadata.name !== undefined && typeof value.metadata.name !== "string"))
  )
    throw new Error("metadata 必须是对象，metadata.name 必须是文字。");
  return value as ExecEnvelope;
}

export function execActive(job: Pick<ExecJob, "status">): boolean {
  return ["queued", "running", "awaiting_approval"].includes(job.status);
}

async function request<T>(method: string, path: string, body: unknown = null): Promise<T> {
  const { call } = await import("./bridge");
  return call<T>("exec_request", { method, path, body });
}

/** The desktop and CLI share the resident executor and its persistent job history. */
export const exec = {
  jobs: () => request<ExecJob[]>("GET", "/v1/exec/jobs"),
  submit: (envelope: ExecEnvelope, output?: string) =>
    request<ExecResponse>("POST", "/v1/exec", { request: envelope, output: output?.trim() || null }),
  job: (id: string) => request<ExecResponse>("GET", `/v1/exec/jobs/${encodeURIComponent(id)}?waitSeconds=0`),
  cancel: (id: string) => request<{ job: ExecJob }>("POST", `/v1/exec/jobs/${encodeURIComponent(id)}/cancel`),
  reveal: (id: string) => request<void>("POST", `/v1/exec/jobs/${encodeURIComponent(id)}/reveal`),
};
