import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
// The release utility runs directly in Node without a TypeScript build.
// @ts-expect-error JavaScript release utility has no declaration file.
import { aggregate, collect, targets, validateVersion } from '../scripts/release.mjs';

test('release omits empty Apple signing variables and preserves configured multiline credentials without logging them', (t) => {
  const workflow = readFileSync(new URL('../.github/workflows/release.yml', import.meta.url), 'utf8');
  const script = workflow.match(/node --input-type=module <<'NODE'\n([\s\S]*?)^\s+NODE\s*$/m)?.[1];
  assert.ok(script, 'the workflow must prepare optional signing values');
  const root = mkdtempSync(join(tmpdir(), 'gizmoth-apple-signing-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const file = join(root, 'github-env');
  const empty = Object.fromEntries(['APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD', 'APPLE_SIGNING_IDENTITY', 'APPLE_API_ISSUER', 'APPLE_API_KEY'].map(name => [name, '']));
  const prepare = (values: Record<string, string>) => {
    writeFileSync(file, '');
    const child = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
      env: { ...process.env, ...empty, ...values, GITHUB_ENV: file }, encoding: 'utf8',
    });
    assert.equal(child.status, 0, child.stderr);
    assert.equal(child.stdout, '', 'signing material must never be logged');
    return readFileSync(file, 'utf8');
  };
  assert.equal(prepare({}), '', 'empty secrets must remain absent in the build step');
  const certificate = 'fixture-certificate\nsecond fixture line';
  const exported = prepare({ APPLE_CERTIFICATE: certificate, APPLE_CERTIFICATE_PASSWORD: 'fixture password', APPLE_SIGNING_IDENTITY: 'fixture identity' });
  assert.equal([...exported.matchAll(/^APPLE_\w+<</gm)].length, 3);
  assert.match(exported, /^APPLE_CERTIFICATE<<([^\n]+)\nfixture-certificate\nsecond fixture line\n\1\n/m);
  assert.match(exported, /^APPLE_CERTIFICATE_PASSWORD<<([^\n]+)\nfixture password\n\1\n/m);
  assert.match(exported, /^APPLE_SIGNING_IDENTITY<<([^\n]+)\nfixture identity\n\1\n/m);
  const build = workflow.split('- name: Build signed updater artifacts without release writes')[1]?.split('- name: Require installers')[0];
  assert.ok(build);
  assert.doesNotMatch(build, /APPLE_(?:CERTIFICATE|CERTIFICATE_PASSWORD|SIGNING_IDENTITY|API_ISSUER|API_KEY):/, 'the build must inherit only prepared credentials');
});

const tag = 'v1.2.3';
const sig = Buffer.from(`untrusted comment: signature from test fixture\n${'A'.repeat(100)}\ntrusted comment: timestamp:1234\n${'A'.repeat(88)}\n`).toString('base64');

function fixture(t: { after: (fn: () => void) => void }) {
  const root = mkdtempSync(join(tmpdir(), 'gizmoth-release-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'src-tauri'), { recursive: true });
  writeFileSync(join(root, 'package.json'), JSON.stringify({ version: '1.2.3' }));
  writeFileSync(join(root, 'src-tauri/Cargo.toml'), '[package]\nname = "gizmoth"\nversion = "1.2.3"\n\n[dependencies]\n');
  writeFileSync(join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ version: '1.2.3', bundle: { createUpdaterArtifacts: true }, plugins: { updater: { pubkey: 'fixture-key' } } }));
  for (const target of Object.keys(targets)) {
    const bundle = join(root, 'src-tauri/target', target, 'release/bundle');
    mkdirSync(bundle, { recursive: true });
    const mac = target.endsWith('apple-darwin');
    const updater = join(bundle, mac ? 'GizMoth.app.tar.gz' : 'GizMoth_1.2.3_x64-setup.exe');
    writeFileSync(updater, `test payload for ${target}`);
    writeFileSync(`${updater}.sig`, sig);
    if (mac) writeFileSync(join(bundle, 'GizMoth_1.2.3.dmg'), 'test dmg');
  }
  return root;
}

function staged(root: string) {
  const input = join(root, 'incoming');
  for (const target of Object.keys(targets)) collect(root, tag, target, join(input, target));
  return input;
}

function changeJson(path: string, change: (value: any) => void) {
  const value = JSON.parse(readFileSync(path, 'utf8'));
  change(value);
  writeFileSync(path, JSON.stringify(value));
}

test('release requires a stable explicit tag and matching versions in all three files', (t) => {
  const root = fixture(t);
  assert.equal(validateVersion(root, tag), '1.2.3');
  for (const invalid of ['main', '', 'v1.2.3-rc.1', 'v01.2.3', 'v1.2.4']) {
    assert.throws(() => validateVersion(root, invalid));
  }
  changeJson(join(root, 'package.json'), (v) => { v.version = '1.2.4'; });
  assert.throws(() => validateVersion(root, tag), /package.json version/);
});

test('Cargo version comes from package section and cannot drift', (t) => {
  const root = fixture(t);
  writeFileSync(join(root, 'src-tauri/Cargo.toml'), '[package]\nversion = "1.2.4"\n[other]\nversion = "1.2.3"\n');
  assert.throws(() => validateVersion(root, tag), /Cargo.toml version/);
});

test('Tauri config requires matching version, enabled updater artifacts and pinned key', (t) => {
  const root = fixture(t);
  const path = join(root, 'src-tauri/tauri.conf.json');
  const initial = readFileSync(path, 'utf8');
  for (const update of [
    (v: any) => { v.version = '1.2.4'; },
    (v: any) => { v.bundle.createUpdaterArtifacts = false; },
    (v: any) => { v.plugins.updater.pubkey = ''; },
  ]) {
    writeFileSync(path, initial);
    changeJson(path, update);
    assert.throws(() => validateVersion(root, tag));
  }
});

test('single aggregate contains all three platform URLs, matching signatures and installers', (t) => {
  const root = fixture(t);
  const output = join(root, 'out');
  const manifest = aggregate(root, tag, staged(root), output);
  assert.deepEqual(Object.keys(manifest.platforms).sort(), Object.values(targets).sort());
  assert.equal(manifest.version, '1.2.3');
  for (const [platform, entry] of Object.entries(manifest.platforms) as [string, any][]) {
    assert.equal(entry.signature, sig);
    assert.match(entry.url, /^https:\/\/github.com\/GizClaw\/gizmoth\/releases\/download\/v1.2.3\/GizMoth_1.2.3_/);
    assert.equal(readFileSync(join(output, entry.url.split('/').at(-1)!), 'utf8').startsWith('test payload'), true);
    assert.equal(entry.url.endsWith(platform.startsWith('darwin') ? '.app.tar.gz' : '-setup.exe'), true);
  }
  assert.equal(JSON.parse(readFileSync(join(output, 'latest.json'), 'utf8')).version, '1.2.3');
});

test('collection fails visibly on missing signatures, invalid signatures and missing installers', (t) => {
  const root = fixture(t);
  const target = 'aarch64-apple-darwin';
  const bundle = join(root, 'src-tauri/target', target, 'release/bundle');
  const path = join(bundle, 'GizMoth.app.tar.gz.sig');
  rmSync(path);
  assert.throws(() => collect(root, tag, target, join(root, 'out')), /Missing updater signature/);
  writeFileSync(path, 'not a signature');
  assert.throws(() => collect(root, tag, target, join(root, 'out')), /Invalid updater signature/);
  writeFileSync(path, Buffer.from('garbage').toString('base64'));
  assert.throws(() => collect(root, tag, target, join(root, 'out')), /Invalid minisign/);
  writeFileSync(path, sig);
  rmSync(join(bundle, 'GizMoth_1.2.3.dmg'));
  assert.throws(() => collect(root, tag, target, join(root, 'out')), /Expected one .dmg/);
});

test('collection rejects ambiguous updater outputs', (t) => {
  const root = fixture(t);
  const target = 'aarch64-apple-darwin';
  writeFileSync(join(root, 'src-tauri/target', target, 'release/bundle/old.app.tar.gz'), 'old');
  assert.throws(() => collect(root, tag, target, join(root, 'out')), /got 2/);
});

for (const corruption of ['missing target', 'mixed version', 'modified payload', 'missing signature', 'unexpected artifact', 'filename traversal']) {
  test(`aggregation refuses ${corruption} before writing latest.json`, (t) => {
    const root = fixture(t);
    const input = staged(root);
    const target = 'aarch64-apple-darwin';
    const dir = join(input, target);
    const metadata = join(dir, `${target}.json`);
    const record = JSON.parse(readFileSync(metadata, 'utf8'));
    if (corruption === 'missing target') rmSync(dir, { recursive: true });
    if (corruption === 'mixed version') changeJson(metadata, (v) => { v.version = '1.2.2'; });
    if (corruption === 'modified payload') writeFileSync(join(dir, record.updater), 'changed');
    if (corruption === 'missing signature') rmSync(join(dir, `${record.updater}.sig`));
    if (corruption === 'unexpected artifact') writeFileSync(join(dir, 'extra.exe'), 'extra');
    if (corruption === 'filename traversal') changeJson(metadata, (v) => { v.updater = '../outside'; });
    const output = join(root, 'out');
    assert.throws(() => aggregate(root, tag, input, output));
    assert.throws(() => readFileSync(join(output, 'latest.json')), /ENOENT/);
  });
}
