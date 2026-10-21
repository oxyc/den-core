import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync,
  writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, parse, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
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

  const sourceSentinel = join(root, 'Cargo.toml');
  const sourceSentinelHash = sha(readFileSync(sourceSentinel));
  const protectedNative = join(cleanRoom, 'protected-native');
  mkdirSync(protectedNative);
  writeFileSync(join(protectedNative, 'keep'), 'unsafe web destinations must not mutate native');
  const assertSourceUntouched = () => {
    assert.equal(sha(readFileSync(sourceSentinel)), sourceSentinelHash);
    assert.equal(readFileSync(join(protectedNative, 'keep'), 'utf8'),
      'unsafe web destinations must not mutate native');
  };

  // Both destinations are preflighted before either is removed. This missing source descendant also exercises
  // canonicalization through the closest existing parent.
  assert.throws(() => vendor({ native: protectedNative, web: join(root, 'missing/vendor') }),
    /unsafe web vendor destination/);
  assertSourceUntouched();

  // Exercise the destructive case against an isolated source tree: without source/destination preflight this
  // would delete its populated crates directory after already deleting protectedNative.
  const isolatedSource = join(cleanRoom, 'isolated-source');
  for (const source of ['Cargo.toml', 'Cargo.lock', 'LICENSE', 'crates', 'bindings', 'scripts',
    'Swift/Package.swift', 'web/index.js', 'web/index.d.ts', 'web/assistant.js', 'web/assistant.d.ts',
    'web/recovery.js', 'web/recovery.d.ts']) {
    const destination = join(isolatedSource, source);
    mkdirSync(dirname(destination), { recursive: true });
    cpSync(join(root, source), destination, { recursive: true });
  }
  mkdirSync(join(isolatedSource, 'Swift/Sources'), { recursive: true });
  mkdirSync(join(isolatedSource, 'Swift/Artifacts'), { recursive: true });
  mkdirSync(join(isolatedSource, 'web/generated'), { recursive: true });
  for (const module of ['den_core', 'den_core_assistant', 'den_core_recovery']) {
    for (const suffix of ['.js', '.d.ts', '_bg.wasm', '_bg.wasm.d.ts']) {
      writeFileSync(join(isolatedSource, 'web/generated', `${module}${suffix}`), 'isolated artifact');
    }
  }
  const isolatedSentinel = join(isolatedSource, 'crates/den-sync/Cargo.toml');
  const isolatedSentinelHash = sha(readFileSync(isolatedSentinel));
  const { vendor: isolatedVendor } = await import(pathToFileURL(join(isolatedSource, 'scripts/vendor.mjs')));
  assert.throws(() => isolatedVendor({ native: protectedNative, web: join(isolatedSource, 'crates') }),
    /unsafe web vendor destination/);
  assert.equal(sha(readFileSync(isolatedSentinel)), isolatedSentinelHash);
  assertSourceUntouched();

  const safeWeb = join(cleanRoom, 'safe-web');
  for (const unsafeNative of [root, join(root, 'missing/native'), dirname(root), parse(root).root]) {
    assert.throws(() => vendor({ native: unsafeNative, web: safeWeb }), /unsafe native vendor destination/);
    assertSourceUntouched();
  }
  assert.throws(() => vendor({ native: protectedNative, web: parse(root).root }),
    /unsafe web vendor destination/);
  assertSourceUntouched();

  assert.throws(() => vendor({ native: join(cleanRoom, 'same'), web: join(cleanRoom, 'same') }),
    /must not overlap/);
  assert.throws(() => vendor({ native: join(cleanRoom, 'nested'), web: join(cleanRoom, 'nested/web') }),
    /must not overlap/);
  assert.throws(() => vendor({ native: join(cleanRoom, 'missing/parent'),
    web: join(cleanRoom, 'missing/parent/web') }), /must not overlap/);

  const sourceAlias = join(cleanRoom, 'source-alias');
  symlinkSync(root, sourceAlias, 'dir');
  assert.throws(() => vendor({ native: protectedNative, web: join(sourceAlias, 'missing/vendor') }),
    /unsafe web vendor destination/);
  assertSourceUntouched();

  const shared = join(cleanRoom, 'shared');
  mkdirSync(shared);
  const nativeAlias = join(cleanRoom, 'native-alias');
  const webAlias = join(cleanRoom, 'web-alias');
  symlinkSync(shared, nativeAlias, 'dir');
  symlinkSync(shared, webAlias, 'dir');
  assert.throws(() => vendor({ native: nativeAlias, web: webAlias }), /must not overlap/);
  assertSourceUntouched();
  console.log('PASS: clean-room native and web vendor packages are deterministic');
} finally {
  rmSync(cleanRoom, { recursive: true, force: true });
}
