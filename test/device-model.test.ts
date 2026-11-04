import assert from "node:assert/strict";
import test from "node:test";
import { deviceModelOf, hardwareSummary, levelOf, parseApps, parseOverview } from "../src/device-model.ts";

const snapshot = {
  online: true,
  lastSeen: null,
  firmwareVersion: "1.0.0",
  errors: [],
  hardware: [
    {
      instance: { id: "speaker.main", hwd: "speaker" },
      value: { volume_percent: 45 },
    },
  ],
};

test("unknown status remains distinct from offline and online", () => {
  assert.equal(parseOverview({ ...snapshot, online: null }).online, null);
  assert.equal(parseOverview({ ...snapshot, online: false }).online, false);
  assert.throws(() => parseOverview({ ...snapshot, online: "true" }));
});
test("missing settings never become a zero percent control", () => {
  assert.equal(
    levelOf({
      instance: { id: "speaker.main", hwd: "speaker" },
      error: "offline",
    }),
    undefined,
  );
  assert.equal(levelOf(parseOverview(snapshot).hardware[0]), 45);
  const result = parseOverview({
    ...snapshot,
    hardware: [
      ...snapshot.hardware,
      {
        instance: { id: "display.main", hwd: "display" },
        error: "Unavailable",
      },
    ],
  });
  assert.equal(result.hardware[0].value?.volume_percent, 45);
  assert.equal(result.hardware[1].error, "Unavailable");
});
test("rejects untrusted values and duplicate hardware identities", () => {
  assert.throws(() =>
    parseOverview({
      ...snapshot,
      hardware: [...snapshot.hardware, ...snapshot.hardware],
    }),
  );
  for (const value of [-1, 101, "45", NaN])
    assert.throws(() =>
      parseOverview({
        ...snapshot,
        hardware: [
          {
            instance: { id: "speaker.main", hwd: "speaker" },
            value: { volume_percent: value },
          },
        ],
      }),
    );
});
test("displays hardware availability and validates installed apps", () => {
  assert.equal(
    hardwareSummary({
      instance: { id: "wifi.main", hwd: "wifi" },
      value: { connected: true, ssid: "Home" },
    }),
    "Home",
  );
  assert.deepEqual(parseApps({ apps: [{ app_id: "hello", version: "1.0.0" }] }), [
    { app_id: "hello", version: "1.0.0", display_name: undefined },
  ]);
  assert.throws(() => parseApps({ apps: [{ app_id: "hello" }] }));
});

test("identifies the model from the firmware board, then the pairing icon", () => {
  assert.equal(deviceModelOf("0.11.7-prod.2+tiga-esp-v4-2")?.id, "h106-tiga");
  assert.equal(deviceModelOf("1.0.0+zero_bk_1_0", "h106_tiga")?.id, "h106-zero");
  assert.equal(deviceModelOf(null, "h106_tiga")?.id, "h106-tiga");
  assert.equal(deviceModelOf("1.0.6", "h106")?.id, "h106-zero");
  assert.equal(deviceModelOf("1.0.6", "ph:robot"), undefined);
  assert.equal(deviceModelOf(), undefined);
});
