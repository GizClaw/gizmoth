import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export const targets = {
  'aarch64-apple-darwin': 'darwin-aarch64',
  'x86_64-apple-darwin': 'darwin-x86_64',
  'x86_64-pc-windows-msvc': 'windows-x86_64',
};
const json = (path) => JSON.parse(readFileSync(path, 'utf8'));
const digest = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
const check = (condition, message) => assert.ok(condition, message);

export function validateVersion(root, tag) {
  // Stable releases only: prereleases must not become the public latest feed.
  check(/^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(tag), 'Expected an explicit stable tag vX.Y.Z');
  const version = tag.slice(1);
  const cargo = readFileSync(join(root, 'src-tauri/Cargo.toml'), 'utf8');
  const packageSection = cargo.match(/^\[package\]\s*\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1];
  const cargoVersion = packageSection?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const config = json(join(root, 'src-tauri/tauri.conf.json'));
  for (const [name, actual] of Object.entries({
    'package.json': json(join(root, 'package.json')).version,
    'Cargo.toml': cargoVersion,
    'tauri.conf.json': config.version,
  })) check(actual === version, `${name} version ${actual} does not match ${tag}`);
  check(config.bundle?.createUpdaterArtifacts === true, 'Release requires createUpdaterArtifacts: true');
  check(config.plugins?.updater?.pubkey?.trim(), 'Release requires a pinned updater public key');
  return version;
}

function files(root) {
  return readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
    const path = join(root, entry.name);
    // The signed archive is the updater payload; do not traverse app frameworks.
    if (entry.isDirectory() && entry.name.endsWith('.app')) return [];
    check(!entry.isSymbolicLink(), `Unexpected symlink: ${path}`);
    return entry.isDirectory() ? files(path) : [path];
  });
}

function signature(path) {
  check(existsSync(path), `Missing updater signature: ${path}`);
  const value = readFileSync(path, 'utf8').trim();
  // Tauri writes a base64-encoded minisign document, not just an arbitrary string.
  check(/^[A-Za-z0-9+/]+={0,2}$/.test(value), `Invalid updater signature: ${path}`);
  const decoded = Buffer.from(value, 'base64').toString('utf8');
  check(/^untrusted comment: .+\r?\n[A-Za-z0-9+/=]+\r?\ntrusted comment: .+\r?\n[A-Za-z0-9+/=]+\s*$/.test(decoded), `Invalid minisign document: ${path}`);
  return value;
}

export function collect(root, tag, target, output) {
  const version = validateVersion(root, tag);
  check(Object.hasOwn(targets, target), `Unknown target ${target}`);
  const mac = target.endsWith('apple-darwin');
  const bundle = join(root, 'src-tauri/target', target, 'release/bundle');
  const candidates = files(bundle);
  const one = (suffix) => {
    const matches = candidates.filter((path) => path.endsWith(suffix));
    check(matches.length === 1, `Expected one ${suffix} for ${target}, got ${matches.length}`);
    check(readFileSync(matches[0]).length > 0, `Empty artifact: ${matches[0]}`);
    return matches[0];
  };
  const updater = one(mac ? '.app.tar.gz' : '-setup.exe');
  const installer = mac ? one('.dmg') : updater;
  const signed = signature(`${updater}.sig`);
  mkdirSync(output, { recursive: true });
  const base = `GizMoth_${version}_${target}`;
  const updaterName = `${base}${mac ? '.app.tar.gz' : '-setup.exe'}`;
  const installerName = mac ? `${base}.dmg` : updaterName;
  copyFileSync(updater, join(output, updaterName));
  if (mac) copyFileSync(installer, join(output, installerName));
  copyFileSync(`${updater}.sig`, join(output, `${updaterName}.sig`));
  const artifacts = [...new Set([updaterName, installerName, `${updaterName}.sig`])];
  const record = { tag, version, target, updater: updaterName, installer: installerName,
    signature: signed, hashes: Object.fromEntries(artifacts.map((name) => [name, digest(join(output, name))])) };
  writeFileSync(join(output, `${target}.json`), `${JSON.stringify(record, null, 2)}\n`);
  return record;
}

export function aggregate(root, tag, input, output) {
  const version = validateVersion(root, tag);
  const platforms = {};
  // Validate the entire set before producing a manifest or copying any assets.
  const records = Object.entries(targets).map(([target, platform]) => {
    const dir = join(input, target);
    const record = json(join(dir, `${target}.json`));
    check(record.tag === tag && record.version === version && record.target === target, `Mixed release metadata for ${target}`);
    const base = `GizMoth_${version}_${target}`;
    const updater = `${base}${target.endsWith('apple-darwin') ? '.app.tar.gz' : '-setup.exe'}`;
    const installer = target.endsWith('apple-darwin') ? `${base}.dmg` : updater;
    check(record.updater === updater && record.installer === installer, `Unexpected filenames for ${target}`);
    const expected = [...new Set([updater, installer, `${updater}.sig`])].sort();
    assert.deepEqual(Object.keys(record.hashes).sort(), expected, `Incomplete artifact set for ${target}`);
    assert.deepEqual(readdirSync(dir).sort(), [...expected, `${target}.json`].sort(), `Unexpected artifacts for ${target}`);
    for (const name of expected) check(digest(join(dir, name)) === record.hashes[name], `Artifact hash mismatch: ${name}`);
    check(signature(join(dir, `${updater}.sig`)) === record.signature, `Signature mismatch for ${target}`);
    platforms[platform] = {
      signature: record.signature,
      url: `https://github.com/GizClaw/gizmoth/releases/download/${tag}/${updater}`,
    };
    return { dir, expected };
  });
  assert.deepEqual(readdirSync(input).sort(), Object.keys(targets).sort(), 'Unexpected or missing target builds');
  mkdirSync(output, { recursive: true });
  check(readdirSync(output).length === 0, 'Release output must be empty');
  for (const { dir, expected } of records) for (const name of expected) copyFileSync(join(dir, name), join(output, name));
  const manifest = { version, notes: `GizMoth ${tag}`, pub_date: new Date().toISOString(), platforms };
  writeFileSync(join(output, 'latest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [command, tag, ...args] = process.argv.slice(2);
  if (command === 'preflight') validateVersion(process.cwd(), tag);
  else if (command === 'collect') collect(process.cwd(), tag, ...args);
  else if (command === 'aggregate') aggregate(process.cwd(), tag, ...args);
  else throw new Error(`Unknown release command: ${command}`);
}
