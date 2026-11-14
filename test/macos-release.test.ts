import test from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
// @ts-expect-error The release verifier runs directly in Node.
import { compileResources, missingCompileResources, validateMacOSSignature } from "../scripts/verify-macos-release.mjs";

// Public fields from an actual Developer ID-signed GizMoth verification build.
const signed = `Identifier=com.gizclaw.gizmoth
CodeDirectory v=20500 size=53119 flags=0x10000(runtime) hashes=1653+3 location=embedded
Authority=Developer ID Application: Yan Dong (SV4P7RNRDK)
Authority=Developer ID Certification Authority
Authority=Apple Root CA
Timestamp=Nov 8, 2026 at 1:12:27 AM
TeamIdentifier=SV4P7RNRDK
Sealed Resources version=2 rules=13 files=46
`;

test("macOS release requires a complete Developer ID bundle for the selected team", () => {
  assert.doesNotThrow(() => validateMacOSSignature(signed));
});

for (const [name, damaged] of [
  ["another signing team", signed.replaceAll("SV4P7RNRDK", "D782F5CP4S")],
  [
    "a development-only identity",
    signed.replace("Authority=Developer ID Application:", "Authority=Apple Development:"),
  ],
  ["another application", signed.replace("Identifier=com.gizclaw.gizmoth", "Identifier=com.example.other")],
  ["ad-hoc signing", signed + "Signature=adhoc\n"],
  ["missing hardened runtime", signed.replace("flags=0x10000(runtime)", "flags=0x0(none)")],
  ["missing signing timestamp", signed.replace(/^Timestamp=.+\n/m, "")],
  [
    "the unsealed resources in v0.1.0",
    signed.replace("Sealed Resources version=2 rules=13 files=46", "Sealed Resources=none"),
  ],
]) {
  test(`macOS release rejects ${name}`, () => {
    assert.throws(() => validateMacOSSignature(damaged));
  });
}

test("the Apple silicon release must bundle OpenVMM, the guest agent and the Linux image", () => {
  const app = mkdtempSync(join(tmpdir(), "gizmoth-app-"));
  try {
    assert.deepEqual(missingCompileResources(app), compileResources);
    for (const path of compileResources as string[]) {
      mkdirSync(dirname(join(app, path)), { recursive: true });
      writeFileSync(join(app, path), "x");
    }
    assert.deepEqual(missingCompileResources(app), []);
    writeFileSync(join(app, "Contents/Resources/linux-image/system.erofs"), "");
    assert.deepEqual(missingCompileResources(app), ["Contents/Resources/linux-image/system.erofs"]);
  } finally {
    rmSync(app, { recursive: true });
  }
});
