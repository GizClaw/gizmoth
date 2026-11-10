import test from "node:test";
import assert from "node:assert/strict";
import type { LogPage, SerialSnapshot, SystemPort } from "../src/serial.ts";

let fixture = 0;
async function withDemo(
  search: string,
  check: (call: (name: string, args: Record<string, unknown>) => unknown) => void,
) {
  const original = Object.getOwnPropertyDescriptor(globalThis, "location");
  Object.defineProperty(globalThis, "location", { configurable: true, value: { search } });
  try {
    const { demoSerial } = await import(new URL(`../src/demo-serial.ts?test=${fixture++}`, import.meta.url).href);
    check(demoSerial);
  } finally {
    if (original) Object.defineProperty(globalThis, "location", original);
    else Reflect.deleteProperty(globalThis, "location");
  }
}

test("empty serial setup discovers OS ports and can add one without configured fixtures", async () => {
  await withDemo("?demo=1&empty", (call) => {
    assert.equal((call("serial_overview", {}) as SerialSnapshot).ports.length, 0);
    const available = call("serial_system_ports", {}) as SystemPort[];
    assert.equal(available.length, 5);
    call("serial_add", { port: { ...available[0], protocol: "serial", baud: 115200, enabled: true } });
    const configured = call("serial_overview", {}) as SerialSnapshot;
    assert.equal(configured.ports.length, 1);
    assert.equal(configured.ports[0].path, available[0].path);
    assert.equal((call("serial_system_ports", {}) as SystemPort[]).length, 5);
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
