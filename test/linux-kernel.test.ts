import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
// @ts-expect-error The staging script runs directly in Node.
import { stageKernel } from "../scripts/stage-linux-kernel.mjs";

// Exercise actual tar extraction and SHA rejection on both release-builder OSes.
for (const valid of [true, false]) {
  test(`kernel staging ${valid ? "extracts only an ARM64 Image" : "rejects an archive checksum mismatch"}`, async () => {
    const root = mkdtempSync(join(tmpdir(), "gizmoth-kernel-"));
    try {
      const input = join(root, "input");
      const output = join(root, "output");
      const cache = join(root, "cache");
      mkdirSync(input);
      const image = Buffer.alloc(128);
      image.writeUInt32LE(0x644d5241, 56);
      writeFileSync(join(input, "Image"), image);
      writeFileSync(join(input, "unrelated"), "must not be extracted");
      const archive = join(root, "kernel.tar.gz");
      execFileSync("tar", ["-czf", archive, "-C", input, "Image", "unrelated"]);
      const sha256 = valid ? createHash("sha256").update(readFileSync(archive)).digest("hex") : "0".repeat(64);
      const options = { output, cache, source: { name: "test kernel", url: pathToFileURL(archive).href, sha256 } };
      if (valid) {
        const manifest = await stageKernel(options);
        assert.deepEqual(readFileSync(join(output, "Image")), image);
        assert.equal(existsSync(join(output, "unrelated")), false);
        assert.equal(manifest.kernel, "test kernel");
        assert.equal(manifest.source.sha256, sha256);
        assert.equal(existsSync(join(output, "system.erofs")), false);
        // Reuse a verified cache with an unreachable URL: no network is needed.
        await stageKernel({ ...options, source: { ...options.source, url: "http://127.0.0.1:1/unreachable" } });
      } else {
        await assert.rejects(stageKernel(options), /does not match its pinned SHA-256/);
        assert.equal(existsSync(join(output, "Image")), false);
        assert.equal(existsSync(join(cache, `${sha256}.partial`)), false);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}
