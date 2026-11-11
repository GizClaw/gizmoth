import test from "node:test";
import assert from "node:assert/strict";
import { copyText } from "../src/clipboard.ts";

async function withClipboard(
  modern: (() => Promise<void>) | undefined,
  fallback: () => boolean,
  check: (state: {
    text: () => string;
    removed: () => boolean;
    restored: () => boolean;
    selected: () => boolean;
  }) => Promise<void>,
) {
  const originals = ["navigator", "document"].map(
    (name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)] as const,
  );
  let removed = false;
  let restored = false;
  let selected = false;
  const textarea = {
    value: "",
    style: { cssText: "" },
    select: () => {
      selected = true;
    },
    remove: () => {
      removed = true;
    },
  };
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: { clipboard: modern ? { writeText: modern } : undefined },
  });
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: {
      activeElement: {
        focus: () => {
          restored = true;
        },
      },
      createElement: () => textarea,
      body: { append: () => {} },
      execCommand: (command: string) => {
        assert.equal(command, "copy");
        assert(selected);
        return fallback();
      },
    },
  });
  try {
    await check({
      text: () => textarea.value,
      removed: () => removed,
      restored: () => restored,
      selected: () => selected,
    });
  } finally {
    for (const [name, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else Reflect.deleteProperty(globalThis, name);
    }
  }
}

test("clipboard API success does not enter the fallback", async () => {
  let calls = 0;
  await withClipboard(
    async () => {
      calls++;
    },
    () => {
      throw new Error("unexpected fallback");
    },
    async (state) => {
      await copyText("Skill");
      assert.equal(calls, 1);
      assert.equal(state.selected(), false);
    },
  );
});

test("clipboard fallback copies the requested content and restores focus", async () => {
  for (const modern of [
    undefined,
    async () => {
      throw new Error("denied");
    },
  ]) {
    await withClipboard(
      modern,
      () => true,
      async (state) => {
        await copyText("Skill\nwith details");
        assert.equal(state.text(), "Skill\nwith details");
        assert(state.removed());
        assert(state.restored());
      },
    );
  }
});

test("failed and throwing clipboard fallbacks still clean up and report failure", async () => {
  for (const fallback of [
    () => false,
    () => {
      throw new Error("denied");
    },
  ]) {
    await withClipboard(undefined, fallback, async (state) => {
      await assert.rejects(copyText("Skill"));
      assert(state.removed());
      assert(state.restored());
    });
  }
});
