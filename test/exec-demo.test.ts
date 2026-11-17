import test from "node:test";
import assert from "node:assert/strict";
import {
  initialExecRequest,
  parseExecRequest,
  type ExecEnvelope,
  type ExecJob,
  type ExecResponse,
} from "../src/exec.ts";

type Call = (command: string, args?: Record<string, unknown>) => unknown;
let fixture = 0;
async function withDemo(
  check: (call: Call, reload: () => Promise<Call>, store: Map<string, string>) => void | Promise<void>,
) {
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  const store = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => store.set(key, value),
    },
  });
  const reload = async (): Promise<Call> => {
    const { demoExec } = await import(new URL(`../src/demo-exec.ts?test=${fixture++}`, import.meta.url).href);
    return demoExec;
  };
  try {
    await check(await reload(), reload, store);
  } finally {
    if (original) Object.defineProperty(globalThis, "localStorage", original);
    else Reflect.deleteProperty(globalThis, "localStorage");
  }
}
const envelope = (spec: Record<string, unknown>): ExecEnvelope => ({
  apiVersion: "gizmoth.dev/v1",
  kind: "Request",
  metadata: { name: "Example execution" },
  spec: spec as ExecEnvelope["spec"],
});
const submit = (call: Call, request: ExecEnvelope, output?: string): ExecResponse =>
  call("exec_request", { method: "POST", path: "/v1/exec", body: { request, output } }) as ExecResponse;
const jobs = (call: Call): ExecJob[] => call("exec_request", { method: "GET", path: "/v1/exec/jobs" }) as ExecJob[];

test("request validation rejects malformed envelopes and preserves accepted JSON fields", () => {
  assert.equal(parseExecRequest(initialExecRequest).spec.operation, "describe");
  for (const invalid of ["{", "null", "[]", "{}", JSON.stringify({ ...envelope({}), spec: [] })])
    assert.throws(() => parseExecRequest(invalid));
  assert.throws(() => parseExecRequest(JSON.stringify({ ...envelope({ operation: "compile" }), metadata: "name" })));
  const request = envelope({
    operation: "compile",
    workspace: "/project with spaces",
    command: ["make", "all"],
    env: { TEXT: '<script>"&' },
  });
  assert.deepEqual(parseExecRequest(JSON.stringify(request)), request);
});

test("capability discovery, job queries and log reads never create or persist task history", async () => {
  await withDemo((call, _reload, store) => {
    const describe = submit(call, parseExecRequest(initialExecRequest));
    assert.equal(describe.job, undefined);
    assert(describe.result);
    assert.deepEqual(submit(call, envelope({ operation: "jobs.list" })), { result: [] });
    assert(submit(call, envelope({ operation: "logs.read", source: "compile", id: "demo" })).result);
    assert.deepEqual(jobs(call), []);
    assert.equal(store.size, 0);
    assert.throws(
      () => submit(call, envelope({ operation: "shell", command: ["echo", "bad"] })),
      /Unsupported operation/,
    );
    assert.deepEqual(jobs(call), []);
  });
});

test("serial discovery and status return current fixtures without creating history", async () => {
  await withDemo((call, _reload, store) => {
    const result = submit(call, envelope({ operation: "serial.list" })).result as { service: string; ports: { id: string }[] };
    assert.equal(result.service, "running");
    assert(result.ports.length > 0);
    const system = submit(call, envelope({ operation: "serial.ports" })).result as { ports: unknown[] };
    assert(system.ports.length > 0);
    const port = result.ports[0];
    assert.deepEqual(submit(call, envelope({ operation: "serial.status", port: port.id })).result, port);
    for (const spec of [
      { operation: "serial.status" },
      { operation: "serial.status", port: "missing" },
      { operation: "serial.status", port: "" },
      { operation: "serial.list", extra: true },
      { operation: "serial.ports", port: "unexpected" },
    ]) assert.throws(() => submit(call, envelope(spec)));
    assert.deepEqual(jobs(call), []);
    assert.equal(store.size, 0);
  });
});

test("accepted JSON and output entries survive module reload without results or logs", async () => {
  await withDemo(async (call, reload, store) => {
    const request = envelope({
      operation: "compile",
      workspace: "/demo/project",
      command: ["make"],
      env: { MODE: "test" },
    });
    const accepted = submit(call, request, "/demo/results/new-result.json").job!;
    request.spec.workspace = "/changed-after-submit";
    const reloaded = await reload();
    const saved = jobs(reloaded)[0];
    assert.equal(saved.id, accepted.id);
    assert.equal(saved.request.spec.workspace, "/demo/project");
    assert.equal(saved.output, "/demo/results/new-result.json");
    assert.equal(submit(reloaded, envelope({ operation: "jobs.get", id: saved.id })).job, undefined);
    assert.equal(jobs(reloaded).length, 1);
    const persisted = JSON.parse([...store.values()][0]) as Record<string, unknown>[];
    for (const key of ["stdout", "logs", "result", "results", "text", "entries"]) assert(!(key in persisted[0]), key);
    saved.request.spec.workspace = "/changed-returned-value";
    assert.equal(jobs(reloaded)[0].request.spec.workspace, "/demo/project");
  });
});

test("demo tasks report final timing and errors after resuming saved history", async (t) => {
  await withDemo(async (call, reload) => {
    const started = Date.now();
    t.mock.method(Date, "now", () => started);
    const success = submit(call, envelope({ operation: "compile", workspace: "/demo", command: ["make"] })).job!;
    const failure = submit(call, envelope({ operation: "compile", workspace: "/demo", command: ["false"] })).job!;
    t.mock.method(Date, "now", () => started + 1000);
    assert(jobs(call).every((job) => job.status === "running" && job.startedAt !== null));
    t.mock.method(Date, "now", () => started + 3000);
    const resumed = jobs(await reload());
    const successful = resumed.find((job) => job.id === success.id)!;
    const failed = resumed.find((job) => job.id === failure.id)!;
    assert.equal(successful.status, "succeeded");
    assert.equal(successful.error, null);
    assert(successful.finishedAt! >= successful.startedAt!);
    assert.equal(failed.status, "failed");
    assert.match(failed.error!, /status 1/);
    assert(failed.finishedAt !== null);
    assert.equal(resumed.length, 2);
  });
});

test("cancellation keeps both the target history and the cancellation action, with no fake output reveal", async () => {
  await withDemo(async (call, reload) => {
    const job = submit(
      call,
      envelope({ operation: "compile", workspace: "/demo", command: ["make"] }),
      "/demo/result.json",
    ).job!;
    const cancelled = call("exec_request", { method: "POST", path: `/v1/exec/jobs/${job.id}/cancel` }) as {
      job: ExecJob;
    };
    assert.equal(cancelled.job.status, "cancelled");
    assert(cancelled.job.finishedAt !== null);
    const persisted = jobs(await reload());
    assert.equal(persisted.length, 2);
    assert.equal(persisted.find((item) => item.operation === "jobs.cancel")!.request.spec.id, job.id);
    assert.equal(persisted.find((item) => item.id === job.id)!.output, "/demo/result.json");
    assert.throws(
      () => call("exec_request", { method: "POST", path: `/v1/exec/jobs/${job.id}/reveal` }),
      /演示模式不会创建输出文件/,
    );
    assert.equal(jobs(call).length, 2);
    const cancellation = envelope({ operation: "jobs.cancel", id: job.id });
    cancellation.metadata = { name: "Keep this cancellation request" };
    const receipt = submit(call, cancellation, "/demo/cancellation.json").job!;
    assert.deepEqual(receipt.request, cancellation);
    assert.equal(receipt.name, "Keep this cancellation request");
    assert.equal(receipt.output, "/demo/cancellation.json");
    assert.deepEqual(jobs(await reload()).find((item) => item.id === receipt.id)!.request, cancellation);
  });
});
