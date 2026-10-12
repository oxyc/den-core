import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { vendor } from './vendor.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
function files(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? files(join(dir, entry.name)) : [join(dir, entry.name)]).sort();
}
function hashes(dir, includeManifest = true) {
  return Object.fromEntries(files(dir).filter((file) => includeManifest || !file.endsWith('/SOURCE.json'))
    .map((file) => [relative(dir, file), sha(readFileSync(file))]));
}
function lockVersion(name) {
  const lock = readFileSync(join(root, 'Cargo.lock'), 'utf8');
  const versions = [...lock.matchAll(/\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"/g)]
    .filter((match) => match[1] === name).map((match) => match[2]);
  assert.equal(versions.length, 1);
  return versions[0];
}
function checkManifest(dir) {
  const manifest = JSON.parse(readFileSync(join(dir, 'SOURCE.json')));
  assert.equal(manifest.uniffi, lockVersion('uniffi'));
  assert.equal(manifest.wasmBindgen, lockVersion('wasm-bindgen'));
  assert.deepEqual(manifest.sources, Object.fromEntries(Object.keys(manifest.sources)
    .map((file) => [file, sha(readFileSync(join(root, file)))])));
  assert.equal(manifest.sourceDigest, sha(JSON.stringify(manifest.sources)));
  assert.deepEqual(manifest.artifacts, hashes(dir, false));
  assert.equal(manifest.artifacts['profiles-v1.json'],
    sha(readFileSync(join(root, 'crates/den-sync/tests/fixtures/profiles-v1.json'))));
  return manifest;
}

const cleanRoom = mkdtempSync(join(tmpdir(), 'den-core-vendor-'));
try {
  const first = { native: join(cleanRoom, 'first/native'), web: join(cleanRoom, 'first/web') };
  const second = { native: join(cleanRoom, 'second/native'), web: join(cleanRoom, 'second/web') };
  vendor(first);
  vendor(second);
  assert.deepEqual(checkManifest(first.native), checkManifest(second.native));
  assert.deepEqual(checkManifest(first.web), checkManifest(second.web));
  assert.deepEqual(hashes(first.native), hashes(second.native));
  assert.deepEqual(hashes(first.web), hashes(second.web));
  writeFileSync(join(first.native, 'stale-artifact'), 'must not enter SOURCE.json');
  writeFileSync(join(first.web, 'stale-artifact'), 'must not enter SOURCE.json');
  vendor(first);
  assert.deepEqual(hashes(first.native), hashes(second.native));
  assert.deepEqual(hashes(first.web), hashes(second.web));
  console.log('PASS: clean-room native and web vendor packages are deterministic');
} finally {
  rmSync(cleanRoom, { recursive: true, force: true });
}
