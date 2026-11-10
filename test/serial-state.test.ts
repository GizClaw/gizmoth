import test from "node:test";
import assert from "node:assert/strict";
import { setImmediate } from "node:timers/promises";
import { SerialLogReader, SerialSnapshotOrder } from "../src/serial-state.ts";
import type { LogEntry, LogPage, SerialSnapshot } from "../src/serial.ts";

const snapshot = (instanceId: string, version: number): SerialSnapshot => ({
  instanceId,
  version,
  running: true,
  h2loaderCore: true,
  ports: [],
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

test("serial revisions reset for a new daemon and reject late replies from retired processes", () => {
  const order = new SerialSnapshotOrder();
  const old = snapshot("A", 500);
  const fresh = snapshot("B", 1);
  assert(order.accepts(null, old, order.begin()));
  assert.equal(order.accepts(old, snapshot("A", 499), order.begin()), false);
  assert(order.accepts(old, fresh, order.begin()));
  assert.equal(order.accepts(fresh, snapshot("A", 501), order.begin()), false);
  assert(order.accepts(fresh, snapshot("B", 2), order.begin()));
});

test("an offline read does not retire a temporarily unavailable daemon", () => {
  const order = new SerialSnapshotOrder();
  const old = snapshot("A", 10);
  const offline = { ...old, instanceId: null, running: false, version: 0 };
  assert(order.accepts(null, old, order.begin()));
  assert(order.accepts(old, offline, order.begin()));
  assert(order.accepts(offline, snapshot("A", 11), order.begin()));
  assert(order.accepts(old, snapshot("B", 1), order.begin()));
});

test("overlapping requests cannot replace a newer daemon with an unseen older one", () => {
  const order = new SerialSnapshotOrder();
  const older = order.begin();
  const newer = order.begin();
  const active = snapshot("B", 1);
  assert(order.accepts(null, active, newer));
  assert.equal(order.accepts(active, snapshot("A", 100), older), false);
  assert.equal(order.accepts(active, { ...active, running: false }, older), false);
  assert(order.accepts(active, snapshot("B", 2), order.begin()));
});

test("an earlier request may publish a newer revision of the same daemon", () => {
  const order = new SerialSnapshotOrder();
  const earlier = order.begin();
  const later = order.begin();
  const current = snapshot("A", 1);
  assert(order.accepts(null, current, later));
  assert(order.accepts(current, snapshot("A", 2), earlier));
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
