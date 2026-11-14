import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmdirSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export const signingTeam = 'SV4P7RNRDK';
const identifier = 'com.gizclaw.gizmoth';

/** Updater signatures do not establish macOS code-signing or notarization trust. */
export function validateMacOSSignature(details) {
  assert.match(details, /^Authority=Developer ID Application: /m, 'A Developer ID Application identity is required');
  assert.match(details, new RegExp(`^TeamIdentifier=${signingTeam}$`, 'm'), 'Unexpected Apple signing team');
  assert.ok(details.split(/\r?\n/).includes(`Identifier=${identifier}`), 'Unexpected application identifier');
  assert.doesNotMatch(details, /\badhoc\b/i, 'Ad-hoc signatures cannot be released');
  assert.match(details, /\bflags=0x[0-9a-f]+\([^)]*\bruntime\b[^)]*\)/i, 'Hardened runtime must be enabled');
  assert.match(details, /^Timestamp=.+$/m, 'A secure signing timestamp is required');
  assert.match(details, /^Sealed Resources version=\d+\b/m, 'The complete app resource seal is required');
}

/** Compile VMs need the OpenVMM sidecar, the guest agent and the Linux image. */
export const compileResources = [
  'Contents/MacOS/openvmm',
  'Contents/Resources/compile-agent/gizmoth-compile-agent',
  'Contents/Resources/linux-image/system.erofs',
  'Contents/Resources/linux-image/Image',
  'Contents/Resources/linux-image/image.json',
];

export function missingCompileResources(app) {
  return compileResources.filter(path => !existsSync(join(app, path)) || statSync(join(app, path)).size === 0);
}

function run(command, args) {
  const result = spawnSync(command, args, { encoding: 'utf8', timeout: 120000 });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${command} failed: ${(result.stdout ?? '') + (result.stderr ?? '')}`);
  return (result.stdout ?? '') + (result.stderr ?? '');
}

export function verifyMacOSApp(app, version, { compile = false } = {}) {
  if (compile) assert.deepEqual(missingCompileResources(app), [], 'The Apple silicon app must bundle compile VMs');
  run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', app]);
  validateMacOSSignature(run('codesign', ['--display', '--verbose=4', app]));
  const actual = run('plutil', ['-extract', 'CFBundleShortVersionString', 'raw', '-o', '-', join(app, 'Contents/Info.plist')]).trim();
  assert.equal(actual, version, 'Unexpected app version');
  run('xcrun', ['stapler', 'validate', app]);
  run('spctl', ['--assess', '--type', 'execute', '--verbose=4', app]);
}

export function verifyMacOSRelease(root, target, { compile = false } = {}) {
  assert.ok(['aarch64-apple-darwin', 'x86_64-apple-darwin'].includes(target), 'Expected a macOS release target');
  const version = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version;
  const bundle = join(root, 'src-tauri/target', target, 'release/bundle');
  verifyMacOSApp(join(bundle, 'macos/GizMoth.app'), version, { compile });
  const diskImages = readdirSync(join(bundle, 'dmg')).filter(name => name.endsWith('.dmg'));
  assert.equal(diskImages.length, 1, 'Expected exactly one macOS installer');
  const image = join(bundle, 'dmg', diskImages[0]);
  run('hdiutil', ['verify', image]);
  const mount = mkdtempSync(join(tmpdir(), 'gizmoth-release-dmg-'));
  let attached = false;
  try {
    run('hdiutil', ['attach', '-readonly', '-nobrowse', '-noautoopen', '-mountpoint', mount, image]);
    attached = true;
    verifyMacOSApp(join(mount, 'GizMoth.app'), version, { compile });
  } finally {
    try {
      if (attached) run('hdiutil', ['detach', mount]);
    } finally {
      // Never traverse a mount if detach fails.
      rmdirSync(mount);
    }
  }
  console.log(`Verified ${target}: Developer ID team ${signingTeam}, resource seal, hardened runtime, timestamp, notarization ticket and Gatekeeper acceptance, including the app inside its DMG.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  verifyMacOSRelease(process.cwd(), process.argv[2], { compile: process.argv.includes('--compile') });
}
