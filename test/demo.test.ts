import test from "node:test";
import assert from "node:assert/strict";
import { demoCall } from "../src/demo.ts";

test("demo push installs and runs the current project instead of throwing run_app", async () => {
  const checked = (await demoCall("project_check", { id: "breathing-light" })) as {
    appId: string;
    bytes: number;
    files: number;
  };
  assert.equal(checked.appId, "breathing_light");
  assert.equal(checked.files, 2);
  assert(checked.bytes > 0);
  await demoCall("project_install", { id: "breathing-light", deviceId: "demo-1" });
  assert.deepEqual(demoCall("run_app", { deviceId: "demo-1", appId: checked.appId }), {
    app_id: "breathing_light",
    running: true,
    simulated: true,
  });
  assert.equal((demoCall("list_apps") as { running_app_id: string }).running_app_id, checked.appId);
  assert.throws(() => demoCall("run_app", { deviceId: "demo-1", appId: "not_installed" }), /尚未安装/);
  assert.throws(() => demoCall("run_app", { deviceId: "missing", appId: checked.appId }), /设备不存在/);
});

test("demo device controls retain accepted values and reject unsupported settings", () => {
  const deviceId = "demo-1";
  assert.deepEqual(
    demoCall("set_device_level", { deviceId, instance: { id: "speaker.main", hwd: "speaker" }, level: 0 }),
    { volume_percent: 0 },
  );
  const overview = demoCall("device_overview", { deviceId }) as {
    hardware: { instance: { hwd: string }; value: { volume_percent?: number } }[];
  };
  assert.equal(overview.hardware.find((item) => item.instance.hwd === "speaker")?.value.volume_percent, 0);
  assert.throws(
    () => demoCall("set_device_level", { deviceId, instance: { id: "display.main", hwd: "display" }, level: 35 }),
    /10%/,
  );
  assert.throws(
    () => demoCall("set_device_level", { deviceId, instance: { id: "mic.main", hwd: "mic" }, level: 40 }),
    /未声明/,
  );
  demoCall("set_device_level", { deviceId, instance: { id: "speaker.main", hwd: "speaker" }, level: 45 });
});

test("demo package validation and install use edited project metadata", async () => {
  const original = demoCall("project_read", { id: "breathing-light", path: "app.json" }) as string;
  try {
    demoCall("project_write", {
      id: "breathing-light",
      path: "app.json",
      content: JSON.stringify({ app_id: "breathing_light", version: "2.0.0", display_name: "Updated" }),
    });
    const result = (await demoCall("project_install", { id: "breathing-light", deviceId: "demo-1" })) as {
      app: { version: string; display_name: string };
    };
    assert.equal(result.app.version, "2.0.0");
    assert.equal(result.app.display_name, "Updated");
    demoCall("project_write", { id: "breathing-light", path: "app.json", content: "{ broken" });
    await assert.rejects(async () => demoCall("project_check", { id: "breathing-light" }));
    await assert.rejects(async () => demoCall("project_install", { id: "breathing-light", deviceId: "demo-1" }));
  } finally {
    demoCall("project_write", { id: "breathing-light", path: "app.json", content: original });
  }
});

test("demo project file operations preserve content and reject destructive rename collisions", async () => {
  const { id } = demoCall("project_create", { appId: "file_test", name: "File workflow" }) as { id: string };
  try {
    demoCall("project_write", { id, path: "file_test/source.lua", content: "return 42\n" });
    demoCall("project_write", { id, path: "file_test/taken.lua", content: "return 7\n" });
    for (const to of ["app.json", "file_test/TAKEN.lua", "../outside.lua"]) {
      assert.throws(() => demoCall("project_rename_file", { id, from: "file_test/source.lua", to }));
      assert.equal(demoCall("project_read", { id, path: "file_test/source.lua" }), "return 42\n");
    }
    demoCall("project_rename_file", { id, from: "file_test/source.lua", to: "file_test/renamed.lua" });
    assert.equal(demoCall("project_read", { id, path: "file_test/renamed.lua" }), "return 42\n");
    assert.throws(() => demoCall("project_read", { id, path: "file_test/source.lua" }), /不存在/);
    assert.throws(() => demoCall("project_remove_file", { id, path: "app.json" }), /不能删除/);
    demoCall("project_remove_file", { id, path: "file_test/renamed.lua" });
    assert.throws(() => demoCall("project_read", { id, path: "file_test/renamed.lua" }), /不存在/);
    await demoCall("project_check", { id });
    demoCall("chat_save", { id, json: "[]" });
  } finally {
    demoCall("project_delete", { id });
  }
  assert.equal(
    (demoCall("projects_list") as { id: string }[]).some((project) => project.id === id),
    false,
  );
  assert.equal(demoCall("chat_load", { id }), null);
});

test("demo device key editing keeps metadata secret-free and rejects invalid replacement", () => {
  const id = "demo-1";
  const original = demoCall("device_key", { id }) as string;
  const replacement = "gizclaw_sk_v1_synthetic_replacement";
  try {
    assert.deepEqual(demoCall("device_key_status", { id }), { hasKey: true, needsMigration: false });
    demoCall("device_save_key", { id, apiKey: replacement });
    assert.equal(demoCall("device_key", { id }), replacement);
    assert.equal(JSON.stringify(demoCall("list_devices")).includes(replacement), false);
    assert.throws(() => demoCall("device_save_key", { id, apiKey: "not-a-valid-key" }), /格式无效/);
    assert.equal(demoCall("device_key", { id }), replacement);
    assert.deepEqual(demoCall("device_migrate_key", { id }), { legacyRemoved: false });
    assert.equal(demoCall("device_key", { id }), replacement);
    assert.throws(() => demoCall("device_key", { id: "missing" }), /未绑定/);
  } finally {
    demoCall("device_save_key", { id, apiKey: original });
  }
});

test("demo project creation keeps duplicate application IDs in distinct projects", () => {
  const first = demoCall("project_create", { appId: "same_app", name: "One" }) as { id: string };
  const second = demoCall("project_create", { appId: "same_app", name: "Two" }) as { id: string };
  try {
    assert.notEqual(first.id, second.id);
    assert.match(demoCall("project_read", { id: first.id, path: "app.json" }) as string, /One/);
    assert.match(demoCall("project_read", { id: second.id, path: "app.json" }) as string, /Two/);
    assert.throws(() => demoCall("project_create", { appId: "../bad", name: "Bad" }));
    assert.throws(() => demoCall("project_create", { appId: "valid", name: " " }));
  } finally {
    demoCall("project_delete", { id: first.id });
    demoCall("project_delete", { id: second.id });
  }
});
