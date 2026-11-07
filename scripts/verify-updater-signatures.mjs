import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { targets } from './release.mjs';

const directory = resolve(process.argv[2] ?? 'release-assets');
const config = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
const manifest = JSON.parse(readFileSync(join(directory, 'latest.json'), 'utf8'));
assert.deepEqual(Object.keys(manifest.platforms).sort(), Object.values(targets).sort());
const temporary = mkdtempSync(join(tmpdir(), 'gizmoth-verify-'));
try {
  const publicKey = join(temporary, 'updater.pub');
  writeFileSync(publicKey, Buffer.from(config.plugins.updater.pubkey, 'base64'));
  for (const [platform, release] of Object.entries(manifest.platforms)) {
    const filename = basename(new URL(release.url).pathname);
    const signature = readFileSync(join(directory, `${filename}.sig`), 'utf8').trim();
    assert.equal(signature, release.signature, `Manifest signature differs for ${platform}`);
    const decoded = join(temporary, `${platform}.minisig`);
    writeFileSync(decoded, Buffer.from(signature, 'base64'));
    execFileSync('minisign', ['-Vm', join(directory, filename), '-p', publicKey, '-x', decoded], { stdio: ['ignore', 'pipe', 'pipe'] });
    console.log(`Verified ${platform} against the public key embedded in the app.`);
  }
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
