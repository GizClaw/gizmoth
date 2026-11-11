import test from "node:test";
import assert from "node:assert/strict";
import type { LogPage, SerialPort, SerialSnapshot, SystemPort } from "../src/serial.ts";

let fixture = 0;
async function withDemo(
  search: string,
  check: (call: (name: string, args: Record<string, unknown>) => unknown) => void | Promise<void>,
) {
  const original = Object.getOwnPropertyDescriptor(globalThis, "location");
  Object.defineProperty(globalThis, "location", { configurable: true, value: { search } });
  try {
    const { demoSerial } = await import(new URL(`../src/demo-serial.ts?test=${fixture++}`, import.meta.url).href);
    await check(demoSerial);
  } finally {
    if (original) Object.defineProperty(globalThis, "location", original);
    else Reflect.deleteProperty(globalThis, "location");
  }
}

test("empty serial setup discovers OS ports and can add one without configured fixtures", async () => {
  await withDemo("?demo=1&empty", (call) => {
    assert.equal((call("serial_overview", {}) as SerialSnapshot).ports.length, 0);
    const available = call("serial_system_ports", {}) as SystemPort[];
    assert.equal(available.length, 12);
    call("serial_add", { port: { ...available[0], protocol: "serial", baud: 115200, enabled: true } });
    const configured = call("serial_overview", {}) as SerialSnapshot;
    assert.equal(configured.ports.length, 1);
    assert.equal(configured.ports[0].path, available[0].path);
    assert.equal((call("serial_system_ports", {}) as SystemPort[]).length, 12);
    call("serial_remove", { id: configured.ports[0].id });
    assert.equal((call("serial_overview", {}) as SerialSnapshot).ports.length, 0);
    assert.deepEqual(call("serial_system_ports", {}), available);
  });
});

test("removing configured ports never changes or mutates OS discovery metadata", async () => {
  await withDemo("?demo=1", (call) => {
    const available = call("serial_system_ports", {}) as SystemPort[];
    for (const port of (call("serial_overview", {}) as SerialSnapshot).ports) call("serial_remove", { id: port.id });
    assert.deepEqual(call("serial_system_ports", {}), available);
    available[0].name = "changed outside fixture";
    assert.notEqual((call("serial_system_ports", {}) as SystemPort[])[0].name, available[0].name);
  });
});

test("the serial fixture stamps owner and observation epochs across overview, watch and service restarts", async (t) => {
  await withDemo("?demo=1", async (call) => {
    const before = call("serial_overview", {}) as SerialSnapshot;
    assert.equal(before.instanceGeneration, 1);
    assert(before.ownerEpoch > 0);
    assert(before.observationEpoch > 0);
    const watched = await (call("serial_watch", {
      since: 0,
      instanceId: before.instanceId,
    }) as Promise<SerialSnapshot>);
    assert.equal(watched.instanceId, before.instanceId);
    assert.equal(watched.instanceGeneration, before.instanceGeneration);
    assert.equal(watched.ownerEpoch, before.ownerEpoch);
    assert(watched.observationEpoch > before.observationEpoch);
    t.mock.timers.enable({ apis: ["setTimeout"] });
    call("serial_stop_service", {});
    const stopped = call("serial_overview", {}) as SerialSnapshot;
    assert.equal(stopped.running, false);
    assert.equal(stopped.instanceGeneration, 0);
    assert(stopped.ownerEpoch > watched.ownerEpoch);
    assert(stopped.observationEpoch > watched.observationEpoch);
    const again = call("serial_overview", {}) as SerialSnapshot;
    assert.equal(again.ownerEpoch, stopped.ownerEpoch);
    assert(again.observationEpoch > stopped.observationEpoch);
    t.mock.timers.tick(4000);
    const restarted = call("serial_overview", {}) as SerialSnapshot;
    assert(restarted.running);
    assert(restarted.ownerEpoch > stopped.ownerEpoch);
    assert(restarted.observationEpoch > again.observationEpoch);
  });
});

test("the serial fixture follows daemon instance and monotonic log-clear generations", async () => {
  await withDemo("?demo=1", (call) => {
    const before = call("serial_overview", {}) as SerialSnapshot;
    assert(before.instanceId);
    const port = before.ports[0];
    const original = call("serial_logs", { id: port.id }) as LogPage;
    assert.equal(original.instanceId, before.instanceId);
    assert.equal(original.generation, 0);
    call("serial_clear_logs", { id: port.id });
    const after = call("serial_overview", {}) as SerialSnapshot;
    const cleared = call("serial_logs", { id: port.id }) as LogPage;
    assert.equal(after.instanceId, before.instanceId);
    assert.equal(after.ports[0].logGeneration, 1);
    assert.equal(cleared.generation, 1);
    assert.equal(cleared.next, original.next);
    assert.equal(cleared.entries.length, 0);
    call("serial_clear_logs", { id: port.id });
    assert.equal((call("serial_logs", { id: port.id }) as LogPage).generation, 2);
  });
});

test("turning a serial connection off and on preserves the WS holder, task and queued connections", async () => {
  await withDemo("?demo=1", (call) => {
    const original = (call("serial_overview", {}) as SerialSnapshot).ports.find((port) => port.id === "demo-uart")!;
    assert.equal(original.holder?.kind, "client");
    assert(original.holder?.kind === "client" && original.holder.title);
    assert(original.queue?.every((request) => request.title));
    assert(original.queue?.some((request) => request.reason === ""));
    const disabled = call("serial_update", { id: original.id, patch: { enabled: false } }) as SerialPort;
    assert.equal(disabled.connection, "off");
    assert.equal(disabled.mode, "piping");
    assert.deepEqual(disabled.holder, original.holder);
    assert.deepEqual(disabled.queue, original.queue);
    const offline = (call("serial_overview", {}) as SerialSnapshot).ports.find((port) => port.id === original.id)!;
    assert.deepEqual(offline.holder, original.holder);
    assert.deepEqual(offline.queue, original.queue);
    const reconnected = call("serial_update", { id: original.id, patch: { enabled: true } }) as SerialPort;
    assert.equal(reconnected.connection, "connected");
    assert.deepEqual(reconnected.holder, original.holder);
    assert.deepEqual(reconnected.queue, original.queue);
  });
});

test("a missing device keeps its WS task and waiters until the selected holder is explicitly released", async () => {
  await withDemo("?demo=1", (call) => {
    const original = (call("serial_overview", {}) as SerialSnapshot).ports.find((port) => port.id === "demo-away")!;
    assert.equal(original.present, false);
    assert.equal(original.connection, "waiting");
    assert(original.holder?.kind === "client" && original.holder.title);
    call("serial_update", { id: original.id, patch: { enabled: false } });
    const waiting = call("serial_update", { id: original.id, patch: { enabled: true } }) as SerialPort;
    assert.equal(waiting.connection, "waiting");
    assert.equal(waiting.connectedAt, null);
    assert.deepEqual(waiting.holder, original.holder);
    assert.deepEqual(waiting.queue, original.queue);
    assert(original.holder?.kind === "client");
    const released = call("serial_release", { id: original.id, connection: original.holder.id }) as SerialPort;
    assert.equal(released.holder, null);
    assert.equal(released.mode, "takeover");
    assert.equal(released.connection, "waiting");
    assert.deepEqual(released.queue, original.queue);
  });
});

test("disconnecting a selected waiting WS leaves the offline holder and other waiters intact", async () => {
  await withDemo("?demo=1", (call) => {
    const original = (call("serial_overview", {}) as SerialSnapshot).ports.find((port) => port.id === "demo-uart")!;
    call("serial_update", { id: original.id, patch: { enabled: false } });
    const [removed, remaining] = original.queue!;
    const denied = call("serial_answer_request", { id: original.id, request: removed.id, accept: false }) as SerialPort;
    assert.equal(denied.connection, "off");
    assert.deepEqual(denied.holder, original.holder);
    assert.deepEqual(denied.queue, [remaining]);
    assert.throws(() => call("serial_answer_request", { id: original.id, request: removed.id, accept: false }));
  });
});

test("explicit admin reclaim disconnects the offline holder, pauses grants and preserves all queued tasks", async () => {
  await withDemo("?demo=1", (call) => {
    const original = (call("serial_overview", {}) as SerialSnapshot).ports.find((port) => port.id === "demo-uart")!;
    call("serial_update", { id: original.id, patch: { autoGrant: true, enabled: false } });
    assert(original.holder?.kind === "client");
    const reclaimed = call("serial_release", {
      id: original.id,
      connection: original.holder.id,
      pauseAutomatic: true,
    }) as SerialPort;
    assert.equal(reclaimed.connection, "off");
    assert.equal(reclaimed.mode, "takeover");
    assert.notEqual(reclaimed.holder?.kind, "client");
    assert.equal(reclaimed.autoGrant, false);
    assert.deepEqual(reclaimed.queue, original.queue);
    const reconnected = call("serial_update", { id: original.id, patch: { enabled: true } }) as SerialPort;
    assert.equal(reconnected.mode, "takeover");
    assert.deepEqual(reconnected.queue, original.queue);
  });
});

test("releasing an offline holder keeps auto-approved tasks queued until the physical port is ready", async () => {
  await withDemo("?demo=1", (call) => {
    const original = (call("serial_overview", {}) as SerialSnapshot).ports.find((port) => port.id === "demo-uart")!;
    call("serial_update", { id: original.id, patch: { autoGrant: true, enabled: false } });
    assert.throws(() =>
      call("serial_answer_request", { id: original.id, request: original.queue![0].id, accept: true }),
    );
    assert(original.holder?.kind === "client");
    const released = call("serial_release", { id: original.id, connection: original.holder.id }) as SerialPort;
    assert.equal(released.connection, "off");
    assert.equal(released.holder, null);
    assert.deepEqual(released.queue, original.queue);
    const reconnected = call("serial_update", { id: original.id, patch: { enabled: true } }) as SerialPort;
    const [next, ...remaining] = original.queue!;
    assert(reconnected.holder?.kind === "client");
    assert.equal(reconnected.mode, "piping");
    assert.equal(reconnected.holder.id, next.id);
    assert.equal(reconnected.holder.title, next.title);
    assert.deepEqual(reconnected.queue, remaining);
  });
});

test("stale holder and waiter disconnects cannot evict a replacement or change automatic approval", async () => {
  await withDemo("?demo=1", (call) => {
    const readPort = () =>
      (call("serial_overview", {}) as SerialSnapshot).ports.find((port) => port.id === "demo-uart")!;
    const original = readPort();
    assert(original.holder?.kind === "client");
    const previousConnection = original.holder.id;
    call("serial_update", { id: original.id, patch: { autoGrant: true } });
    const replacement = call("serial_release", { id: original.id, connection: previousConnection }) as SerialPort;
    assert(replacement.holder?.kind === "client");
    assert.notEqual(replacement.holder.id, previousConnection);
    assert.throws(() =>
      call("serial_release", {
        id: original.id,
        connection: previousConnection,
        pauseAutomatic: true,
      }),
    );
    const afterStale = readPort();
    assert.deepEqual(afterStale.holder, replacement.holder);
    assert.deepEqual(afterStale.queue, replacement.queue);
    assert.equal(afterStale.autoGrant, true);
    assert.equal(afterStale.mode, "piping");
    const waiter = replacement.queue![0];
    const disconnected = call("serial_release", { id: original.id, connection: waiter.id }) as SerialPort;
    assert.deepEqual(disconnected.holder, replacement.holder);
    assert.deepEqual(disconnected.queue, []);
    assert.throws(() =>
      call("serial_release", {
        id: original.id,
        connection: waiter.id,
        pauseAutomatic: true,
      }),
    );
    const afterStaleWaiter = readPort();
    assert.deepEqual(afterStaleWaiter.holder, replacement.holder);
    assert.deepEqual(afterStaleWaiter.queue, []);
    assert.equal(afterStaleWaiter.autoGrant, true);
  });
});

test("explicit release hands communication to the next queued task when automatic approval is enabled", async () => {
  await withDemo("?demo=1", (call) => {
    const original = (call("serial_overview", {}) as SerialSnapshot).ports.find((port) => port.id === "demo-uart")!;
    call("serial_update", { id: original.id, patch: { autoGrant: true } });
    assert(original.holder?.kind === "client");
    const released = call("serial_release", { id: original.id, connection: original.holder.id }) as SerialPort;
    const [next, ...remaining] = original.queue!;
    assert(released.holder?.kind === "client");
    assert.equal(released.mode, "piping");
    assert.equal(released.holder.id, next.id);
    assert.equal(released.holder.client, next.client);
    assert.equal(released.holder.title, next.title);
    assert.equal(released.holder.reason, next.reason);
    assert.deepEqual(released.queue, remaining);
  });
});
