import { createServer } from "node:http";

function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

/** A queued, loopback-only OpenAI SSE endpoint. Unscripted requests fail closed. */
export async function startMockModel() {
  const queue = [];
  const requests = [];
  const unexpected = [];
  const responses = new Set();
  const server = createServer((request, response) => {
    void handle(request, response).catch((error) => {
      unexpected.push(String(error));
      if (!response.headersSent) response.writeHead(400, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { message: "Invalid synthetic request" } }));
    });
  });

  async function handle(request, response) {
    if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
      unexpected.push(`${request.method} ${request.url}`);
      response.writeHead(404).end();
      return;
    }
    const chunks = [];
    let bytes = 0;
    for await (const chunk of request) {
      bytes += chunk.length;
      if (bytes > 1024 * 1024) throw new Error("Synthetic request exceeds 1 MiB");
      chunks.push(chunk);
    }
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    const captured = { method: request.method, url: request.url, body };
    requests.push(captured);
    const plan = queue.shift();
    if (!plan) {
      unexpected.push("Unscripted model request");
      response.writeHead(400, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { message: "No synthetic response was queued" } }));
      return;
    }
    responses.add(response);
    response.once("close", () => {
      responses.delete(response);
      plan.disconnected = !response.writableEnded;
      plan.closed.resolve();
    });
    plan.request = captured;
    plan.started.resolve(captured);
    if (plan.error) {
      response.writeHead(400, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { message: plan.error, type: "synthetic_test_error" } }));
      return;
    }
    response.writeHead(200, {
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
      connection: "keep-alive",
    });
    response.flushHeaders();
    const chunk = (delta, finishReason = null) => {
      if (!response.destroyed)
        response.write(
          `data: ${JSON.stringify({
            id: `chatcmpl-e2e-${requests.length}`,
            object: "chat.completion.chunk",
            created: 1,
            model: body.model,
            choices: [{ index: 0, delta, finish_reason: finishReason }],
          })}\n\n`,
        );
    };
    if (!plan.silent) {
      chunk({ role: "assistant" });
      if (plan.thinking) chunk({ reasoning_content: plan.thinking });
    }
    if (plan.hold) {
      // Standard SSE comments keep the transport active without producing model
      // content. tauri-plugin-http 2.8 retains an in-flight reqwest body read
      // after cancellation until it receives a chunk, so include heartbeats.
      const heartbeat =
        plan.heartbeat === false
          ? undefined
          : setInterval(() => {
              if (!response.destroyed) response.write(": synthetic heartbeat\n\n");
            }, 100);
      try {
        await Promise.race([plan.released.promise, plan.closed.promise]);
      } finally {
        clearInterval(heartbeat);
      }
    }
    if (response.destroyed) return;
    if (plan.tools) {
      chunk({
        tool_calls: plan.tools.map((tool, index) => ({
          index,
          id: tool.id,
          type: "function",
          function: { name: tool.name, arguments: JSON.stringify(tool.arguments) },
        })),
      });
      chunk({}, "tool_calls");
    } else {
      chunk({ content: plan.text ?? "Synthetic response complete." });
      chunk({}, "stop");
    }
    response.end("data: [DONE]\n\n");
  }

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const { port } = server.address();
  return {
    url: `http://127.0.0.1:${port}/v1`,
    requests,
    unexpected,
    enqueue(options = {}) {
      const plan = { ...options, started: deferred(), released: deferred(), closed: deferred() };
      queue.push(plan);
      return {
        started: plan.started.promise,
        closed: plan.closed.promise,
        release: () => plan.released.resolve(),
        get request() {
          return plan.request;
        },
        get disconnected() {
          return plan.disconnected;
        },
      };
    },
    pending: () => queue.length,
    async close() {
      for (const response of responses) response.destroy();
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    },
  };
}
