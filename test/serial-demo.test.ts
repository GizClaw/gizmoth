import test from "node:test";
import assert from "node:assert/strict";
import type { LogPage, RemoteProbe, SerialSnapshot, SystemPort } from "../src/serial.ts";

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

test("imported GizMoth nodes connect on the LAN with an address and API key", async () => {
  await withDemo("?demo=1", (call) => {
    const before = call("serial_overview", {}) as SerialSnapshot;
    const nodes = before.nodes ?? [];
    assert.equal(nodes.length, 2);
    const [online, offline] = nodes;
    assert.equal(online.status, "online");
    for (const port of online.ports) {
      assert.equal(port.node, online.id);
      assert(port.id.startsWith(`${online.id}--`), "imported IDs carry their node");
      assert(!before.ports.some((local) => local.id === port.id));
    }
    assert(offline.ports.every((port) => port.remote?.stale));
    assert.throws(() => call("serial_send", { id: offline.ports[0].id, text: "x" }), /未连接/);

    const probe = call("serial_remote_probe", { url: "192.168.1.70", apiKey: "gmk_example-key" }) as RemoteProbe;
    assert.equal(probe.url, "http://192.168.1.70:7420");
    assert(probe.node);
    assert.equal("certificate" in probe, false);
    assert.throws(() => call("serial_remote_probe", { url: "192.168.1.70", apiKey: "bad" }), /API Key/);
    const added = call("serial_remote_add", { url: "192.168.1.70", apiKey: "gmk_example-key" }) as { id: string };
    assert.equal((call("serial_overview", {}) as SerialSnapshot).nodes?.length, 3);
    call("serial_remote_remove", { id: added.id });
    assert.equal((call("serial_overview", {}) as SerialSnapshot).nodes?.length, 2);
  });
});

test("a command on an imported port takes control there unless another client holds it", async () => {
  await withDemo("?demo=1", (call) => {
    const [online] = (call("serial_overview", {}) as SerialSnapshot).nodes ?? [];
    const [free, held] = online.ports;
    call("serial_job", { id: free.id, kind: "refresh" });
    const granted = (call("serial_overview", {}) as SerialSnapshot).nodes![0].ports[0];
    assert.equal(granted.remote?.control.state, "granted");
    call("serial_release", { id: free.id });
    assert.equal((call("serial_overview", {}) as SerialSnapshot).nodes![0].ports[0].remote?.control.state, "none");
    assert.throws(() => call("serial_send", { id: held.id, text: "x" }), /排队/);
    assert.equal((call("serial_overview", {}) as SerialSnapshot).nodes![0].ports[1].remote?.control.state, "queued");
  });
});

test("LAN service needs no certificate configuration", async () => {
  await withDemo("?demo=1", (call) => {
    const status = call("gateway_update", { patch: { scope: "lan" } }) as { urls: string[] };
    assert(status.urls.some((url) => url.startsWith("http://192.168.")));
    assert(status.urls.some((url) => url.startsWith("http://100.")));
    assert(status.urls.every((url) => url.startsWith("http://")));
    assert.equal("tlsCertificateChain" in status, false);
    const local = call("gateway_update", { patch: { scope: "local" } }) as { urls: string[] };
    assert.deepEqual(local.urls, ["http://127.0.0.1:7420"]);
  });
});
