import test from "node:test";
import assert from "node:assert/strict";
import { setImmediate } from "node:timers/promises";
import { SerialLogReader, SerialSnapshotOrder } from "../src/serial-state.ts";
import type { LogEntry, LogPage, SerialSnapshot } from "../src/serial.ts";

const snapshot = (
  ownerEpoch: number,
  observationEpoch: number,
  state: Partial<SerialSnapshot> = {},
): SerialSnapshot => ({
  instanceId: "A",
  instanceGeneration: 1,
  version: 1,
  running: true,
  h2loaderCore: true,
  ports: [],
  ...state,
  ownerEpoch,
  observationEpoch,
});
const entries = (from: number, to: number): LogEntry[] =>
  Array.from({ length: Math.max(0, to - from + 1) }, (_, i) => ({
    seq: from + i,
    at: 0,
    source: "rx",
    text: `line ${from + i}`,
  }));
const page = (from: number, to: number, generation = 0, instanceId = "A"): LogPage => ({
  instanceId,
  generation,
  next: to,
  truncated: false,
  entries: entries(from, to),
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => (resolve = done));
  return { promise, resolve };
}
function harness() {
  const requests: { after?: number; limit?: number; resolve: (page: LogPage) => void }[] = [];
  const reader = new SerialLogReader((_id, after, limit) => {
    const response = deferred<LogPage>();
    requests.push({ after, limit, resolve: response.resolve });
    return response.promise;
  });
  const stop = reader.subscribe(() => {});
  const update = (seq: number, generation = 0, instanceId = "A") =>
    reader.update({ id: "port", running: true, instanceId, generation, seq });
  return { reader, requests, stop, update };
}

const offline = (owner: number, observation: number) =>
  snapshot(owner, observation, {
    running: false,
    instanceId: null,
    instanceGeneration: 0,
    version: 0,
    h2loaderCore: false,
  });

test("a verified legacy owner can replace a modern daemon without admitting its late replies", () => {
  const order = new SerialSnapshotOrder();
  const modern = snapshot(1, 1, { instanceGeneration: 5, version: 500 });
  const legacy = snapshot(2, 2, { instanceId: "legacy", instanceGeneration: 0 });
  assert(order.accepts(modern));
  assert(order.accepts(legacy));
  assert.equal(order.accepts({ ...modern, version: 501 }), false);
  assert(order.accepts({ ...legacy, observationEpoch: 3, version: 2 }));
});

test("a verified modern owner can replace a legacy daemon with its own revision sequence", () => {
  const order = new SerialSnapshotOrder();
  const legacy = snapshot(1, 1, { instanceId: "legacy", instanceGeneration: 0, version: 500 });
  assert(order.accepts(legacy));
  assert(order.accepts(snapshot(2, 2)));
  assert.equal(order.accepts({ ...legacy, version: 501 }), false);
});

test("successive verified legacy owners can replace one another", () => {
  const order = new SerialSnapshotOrder();
  const first = snapshot(1, 1, { instanceId: "legacy-A", instanceGeneration: 0, version: 100 });
  assert(order.accepts(first));
  assert(order.accepts(snapshot(2, 2, { instanceId: "legacy-B", instanceGeneration: 0 })));
  assert.equal(order.accepts({ ...first, version: 101 }), false);
});

test("an older observation cannot overwrite the current owner even with a higher daemon generation", () => {
  const order = new SerialSnapshotOrder();
  assert(order.accepts(snapshot(2, 20, { instanceId: "legacy", instanceGeneration: 0 })));
  assert.equal(order.accepts(snapshot(1, 19, { instanceGeneration: 100, version: 500 })), false);
  assert.equal(order.accepts(offline(1, 18)), false);
});

test("observation order and owner identity must both move forward", () => {
  const order = new SerialSnapshotOrder();
  assert(order.accepts(snapshot(2, 20)));
  assert.equal(order.accepts(snapshot(1, 21)), false);
  assert.equal(order.accepts(snapshot(3, 19, { instanceId: "B" })), false);
  assert.equal(order.accepts(snapshot(2, 20)), false);
  assert(order.accepts(snapshot(3, 21, { instanceId: "B", instanceGeneration: 2 })));
});

test("same-owner observations retain UUID, generation and revision watermarks", () => {
  const order = new SerialSnapshotOrder();
  assert(order.accepts(snapshot(1, 1, { version: 10 })));
  assert.equal(order.accepts(snapshot(1, 2, { version: 9 })), false);
  assert.equal(order.accepts(snapshot(1, 3, { instanceId: "B", version: 11 })), false);
  assert.equal(order.accepts(snapshot(1, 4, { instanceGeneration: 2, version: 11 })), false);
  assert(order.accepts(snapshot(1, 5, { version: 11 })));
  assert(order.accepts(snapshot(1, 6, { version: 11 })));
});

test("same-owner legacy observations also reject stale versions without requiring a UUID", () => {
  const order = new SerialSnapshotOrder();
  const legacy = { instanceId: null, instanceGeneration: 0, version: 10 };
  assert(order.accepts(snapshot(1, 1, legacy)));
  assert.equal(order.accepts(snapshot(1, 2, { ...legacy, version: 9 })), false);
  assert(order.accepts(snapshot(1, 3, { ...legacy, version: 11 })));
});

test("offline observations block earlier same-instance replies and retain the same-owner version", () => {
  const order = new SerialSnapshotOrder();
  assert(order.accepts(snapshot(1, 1, { version: 10 })));
  assert(order.accepts(offline(1, 3)));
  assert.equal(order.accepts(snapshot(1, 2, { version: 11 })), false);
  assert.equal(order.accepts(snapshot(1, 4, { version: 9 })), false);
  assert.equal(order.accepts(snapshot(1, 5, { instanceId: "B", instanceGeneration: 0 })), false);
  assert(order.accepts(snapshot(1, 6, { version: 10 })));
  assert.equal(order.accepts(offline(1, 3)), false);
});

test("an offline owner can be replaced by a newly verified legacy owner", () => {
  const order = new SerialSnapshotOrder();
  const modern = snapshot(1, 1, { instanceGeneration: 5, version: 500 });
  assert(order.accepts(modern));
  assert(order.accepts(offline(2, 2)));
  assert.equal(order.accepts({ ...modern, version: 501 }), false);
  assert(order.accepts(snapshot(3, 3, { instanceId: "legacy", instanceGeneration: 0 })));
  assert.equal(order.accepts(offline(2, 2)), false);
});

test("rediscovering the same process preserves its revision across an absent owner", () => {
  const order = new SerialSnapshotOrder();
  assert(order.accepts(snapshot(1, 1, { version: 10 })));
  assert(order.accepts(offline(2, 2)));
  assert.equal(order.accepts(snapshot(3, 3, { version: 9 })), false);
  assert(order.accepts(snapshot(3, 4, { version: 10 })));
});

test("a daemon keeps its revision watermark when its discovery port changes A to B to A", () => {
  const order = new SerialSnapshotOrder();
  const portA = { enabled: true, scope: "local" as const, port: 7420 };
  const portB = { ...portA, port: 7421 };
  assert(order.accepts(snapshot(1, 1, { version: 100, gateway: portA })));
  assert(order.accepts(snapshot(2, 2, { version: 200, gateway: portB })));
  // An old read on port A is verified after the port returns to A, giving it
  // fresh IPC epochs but an obsolete revision from the same daemon process.
  assert.equal(order.accepts(snapshot(3, 3, { version: 150, gateway: portA })), false);
  assert(order.accepts(snapshot(3, 4, { version: 201, gateway: portA })));
  assert(order.accepts(snapshot(4, 5, { instanceId: "legacy", instanceGeneration: 0, gateway: portA })));
});

test("invalid IPC epochs and invalid modern process identities cannot replace valid snapshots", () => {
  const order = new SerialSnapshotOrder();
  assert.equal(order.accepts(snapshot(0, 1)), false);
  assert.equal(order.accepts(snapshot(1, 0)), false);
  assert.equal(order.accepts(snapshot(1, Number.MAX_SAFE_INTEGER + 1)), false);
  assert.equal(order.accepts(snapshot(1, 1, { instanceGeneration: undefined! })), false);
  assert.equal(order.accepts(snapshot(1, 1, { instanceId: null })), false);
  assert(order.accepts(snapshot(1, 1)));
});

test("an independent log page may run ahead of its overview without clearing loaded history", async () => {
  const h = harness();
  try {
    h.update(600);
    h.requests[0].resolve(page(1, 600));
    await setImmediate();
    h.update(800);
    assert.equal(h.requests[1].after, 600);
    h.requests[1].resolve(page(601, 801));
    await setImmediate();
    assert.equal(h.reader.read().entries.length, 801);
    assert.equal(h.reader.read().entries[0].seq, 1);
    h.update(800);
    h.update(801);
    assert.equal(h.requests.length, 2);
  } finally {
    h.stop();
  }
});

test("forward watermarks do not invalidate a slow in-flight read", async () => {
  const h = harness();
  try {
    h.update(10);
    h.requests[0].resolve(page(1, 10));
    await setImmediate();
    h.update(11);
    for (let seq = 12; seq <= 20; seq++) h.update(seq);
    assert.equal(h.requests.length, 2);
    h.requests[1].resolve(page(11, 15));
    await setImmediate();
    assert.equal(h.requests.length, 3);
    assert.equal(h.requests[2].after, 15);
    h.requests[2].resolve(page(16, 20));
    await setImmediate();
    assert.deepEqual(
      h.reader.read().entries.map((entry) => entry.seq),
      entries(1, 20).map((entry) => entry.seq),
    );
  } finally {
    h.stop();
  }
});

test("daemon replacement invalidates old pending log responses", async () => {
  const h = harness();
  try {
    h.update(900);
    h.update(1, 0, "B");
    assert.equal(h.requests.length, 2);
    assert.equal(h.requests[1].after, undefined);
    h.requests[0].resolve(page(898, 900));
    h.requests[1].resolve(page(1, 1, 0, "B"));
    await setImmediate();
    assert.deepEqual(
      h.reader.read().entries.map((entry) => entry.seq),
      [1],
    );
  } finally {
    h.stop();
  }
});

test("a log page can discover a clear before the overview without rolling its generation back", async () => {
  const h = harness();
  try {
    h.update(4);
    h.requests[0].resolve(page(1, 4));
    await setImmediate();
    h.update(5);
    h.requests[1].resolve(page(5, 5, 1));
    await setImmediate();
    assert.equal(h.requests.length, 3);
    assert.equal(h.requests[2].after, undefined);
    assert.equal(h.reader.read().entries.length, 0);
    h.update(5, 0);
    assert.equal(h.requests.length, 3);
    h.requests[2].resolve(page(5, 5, 1));
    await setImmediate();
    h.update(5, 1);
    assert.deepEqual(
      h.reader.read().entries.map((entry) => entry.seq),
      [5],
    );
    assert.equal(h.requests.length, 3);
  } finally {
    h.stop();
  }
});

test("clearing a log invalidates every mounted reader even if its sequence is unchanged", async () => {
  const readers = [harness(), harness()];
  try {
    for (const h of readers) {
      h.update(4);
      h.requests[0].resolve(page(1, 4));
    }
    await setImmediate();
    for (const h of readers) {
      h.update(4, 1);
      assert.equal(h.requests[1].after, undefined);
      h.requests[1].resolve(page(5, 4, 1));
    }
    await setImmediate();
    for (const h of readers) assert.equal(h.reader.read().entries.length, 0);
  } finally {
    for (const h of readers) h.stop();
  }
});

test("a generation change discards the old pending page and keeps the new empty log", async () => {
  const h = harness();
  try {
    h.update(9);
    h.update(9, 1);
    h.requests[1].resolve(page(10, 9, 1));
    await setImmediate();
    h.requests[0].resolve(page(1, 9));
    await setImmediate();
    assert.equal(h.reader.read().entries.length, 0);
    assert.equal(h.requests.length, 2);
  } finally {
    h.stop();
  }
});

test("unmounting ignores pending replies and does not start follow-up reads", async () => {
  const h = harness();
  h.update(100);
  h.stop();
  h.requests[0].resolve(page(1, 10));
  await setImmediate();
  assert.equal(h.reader.read().entries.length, 0);
  assert.equal(h.requests.length, 1);
});

test("a non-progressing page does not busy-loop and retains the existing log", async () => {
  const h = harness();
  try {
    h.update(1);
    h.requests[0].resolve(page(1, 1));
    await setImmediate();
    h.update(2);
    h.requests[1].resolve(page(2, 1));
    await setImmediate();
    assert.equal(h.requests.length, 2);
    assert.equal(h.reader.read().entries.length, 1);
  } finally {
    h.stop();
  }
});
