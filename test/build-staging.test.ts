import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
// @ts-expect-error The build scripts run directly in Node.
import { buildCompileAgent } from "../scripts/build-compile-agent.mjs";
// @ts-expect-error The build scripts run directly in Node.
import { verifyPatchedTree } from "../scripts/build-openvmm.mjs";
// @ts-expect-error The staging script runs directly in Node.
import { stageKernel } from "../scripts/stage-linux-kernel.mjs";

for (const kind of ["valid", "truncated", "wrong magic"]) {
  test(`kernel header validation: ${kind}`, async () => {
    const temporary = mkdtempSync(join(tmpdir(), "gizmoth-kernel-header-"));
    try {
      const input = join(temporary, "input");
      mkdirSync(input);
      const image = Buffer.alloc(kind === "truncated" ? 63 : 128);
      image.writeUInt32LE(kind === "wrong magic" ? 0 : 0x644d5241, 56);
      writeFileSync(join(input, "Image"), image);
      const archive = join(temporary, "kernel.tar.gz");
      execFileSync("tar", ["-czf", archive, "-C", input, "Image"]);
      const options = {
        cache: join(temporary, "cache"),
        output: join(temporary, "output"),
        source: { name: "test", url: pathToFileURL(archive).href, sha256: createHash("sha256").update(readFileSync(archive)).digest("hex") },
      };
      if (kind === "valid") {
        const info = await stageKernel(options);
        assert.equal(info.sha256, createHash("sha256").update(image).digest("hex"));
        assert.deepEqual(readFileSync(join(options.output, "Image")), image);
      } else {
        await assert.rejects(stageKernel(options), /not an ARM64 Linux Image/);
      }
    } finally {
      rmSync(temporary, { recursive: true, force: true });
    }
  });
}

for (const setting of ["environment", "configuration"]) {
  test(`compile-agent stages Cargo's executable with target directory from ${setting}`, () => {
    const temporary = mkdtempSync(join(tmpdir(), "gizmoth-agent-staging-"));
    const originalPath = process.env.PATH;
    const originalTarget = process.env.CARGO_TARGET_DIR;
    try {
      const crate = join(temporary, "crate");
      const bin = join(temporary, "bin");
      const target = join(temporary, "custom-target");
      const executable = join(target, "aarch64-unknown-linux-musl/release/gizmoth-compile-agent");
      mkdirSync(bin);
      mkdirSync(join(crate, ".cargo"), { recursive: true });
      mkdirSync(join(crate, "target/aarch64-unknown-linux-musl/release"), { recursive: true });
      mkdirSync(join(target, "aarch64-unknown-linux-musl/release"), { recursive: true });
      writeFileSync(executable, "fresh executable");
      writeFileSync(join(crate, "target/aarch64-unknown-linux-musl/release/gizmoth-compile-agent"), "stale executable");
      if (setting === "environment") process.env.CARGO_TARGET_DIR = target;
      else {
        delete process.env.CARGO_TARGET_DIR;
        writeFileSync(join(crate, ".cargo/config.toml"), `[build]\ntarget-dir = ${JSON.stringify(target)}\n`);
      }
      // A Cargo stand-in reports the same JSON protocol as real Cargo. Its
      // executable deliberately differs from the conventional target path.
      const cargo = join(bin, "cargo");
      writeFileSync(cargo, `#!/usr/bin/env node
import assert from "node:assert/strict";
assert.ok(process.argv.includes("--message-format=json-render-diagnostics"));
assert.equal(process.env.CARGO_TARGET_DIR, ${setting === "environment" ? JSON.stringify(target) : "undefined"});
console.log(JSON.stringify({reason:"compiler-artifact",target:{name:"dependency",kind:["lib"]},executable:null}));
console.log(JSON.stringify({reason:"compiler-artifact",target:{name:"gizmoth-compile-agent",kind:["bin"]},executable:${JSON.stringify(executable)},fresh:true}));
console.log(JSON.stringify({reason:"build-finished",success:true}));
`);
      chmodSync(cargo, 0o755);
      process.env.PATH = `${bin}:${originalPath}`;
      const output = buildCompileAgent({ crate, destination: join(temporary, "staged") });
      assert.equal(readFileSync(output, "utf8"), "fresh executable");
    } finally {
      if (originalPath === undefined) delete process.env.PATH;
      else process.env.PATH = originalPath;
      if (originalTarget === undefined) delete process.env.CARGO_TARGET_DIR;
      else process.env.CARGO_TARGET_DIR = originalTarget;
      rmSync(temporary, { recursive: true, force: true });
    }
  });
}

test("OpenVMM patch verification ignores diff display settings and preserves the real index", () => {
  const temporary = mkdtempSync(join(tmpdir(), "gizmoth-patch-check-"));
  const source = join(temporary, "checkout");
  mkdirSync(source);
  const git = (...args: string[]) => execFileSync("git", ["-C", source, ...args], { encoding: "utf8" });
  try {
    git("init", "-q");
    writeFileSync(join(source, "tracked"), "original\n");
    git("add", "tracked");
    const tree = git("write-tree").trim();
    writeFileSync(join(source, "tracked"), "patched\n");
    writeFileSync(join(source, "added"), "new patched file\n");
    git("add", "tracked", "added");
    const patch = join(temporary, "expected.patch");
    writeFileSync(patch, git("diff", "--binary", "--cached", tree, "--"));
    git("read-tree", tree);
    const index = readFileSync(join(source, ".git/index"));
    git("config", "core.abbrev", "12");
    git("config", "diff.noprefix", "true");
    git("config", "diff.external", "this-command-must-never-run");
    verifyPatchedTree(source, patch, tree);
    assert.deepEqual(readFileSync(join(source, ".git/index")), index);
    assert.equal(readFileSync(join(source, "tracked"), "utf8"), "patched\n");
    writeFileSync(join(source, "tracked"), "unexpected change\n");
    assert.throws(() => verifyPatchedTree(source, patch, tree), /differs from the pinned/);
    assert.deepEqual(readFileSync(join(source, ".git/index")), index);
    writeFileSync(join(source, "tracked"), "patched\n");
    writeFileSync(join(source, "untracked"), "must be preserved\n");
    assert.throws(() => verifyPatchedTree(source, patch, tree), /differs from the pinned/);
    assert.equal(readFileSync(join(source, "untracked"), "utf8"), "must be preserved\n");
    assert.deepEqual(readFileSync(join(source, ".git/index")), index);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
});
