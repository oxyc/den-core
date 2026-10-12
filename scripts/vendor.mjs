// Mechanical packaging only. Clients consume immutable generated artifacts, never a sibling at runtime/build.
import { createHash } from 'node:crypto';
import { accessSync, cpSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('..', import.meta.url));
const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
function files(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? files(join(dir, entry.name)) : [join(dir, entry.name)]).sort();
}
const inputs = ['Cargo.toml', 'Cargo.lock', ...files(join(root, 'crates')).map((f) => relative(root, f)),
  ...files(join(root, 'bindings')).map((f) => relative(root, f)),
  'scripts/build-bindings.sh', 'scripts/test-vendor.mjs', 'scripts/vendor.mjs',
  'Swift/Package.swift', 'web/index.js', 'web/index.d.ts', 'web/assistant.js', 'web/assistant.d.ts'];
inputs.push('LICENSE');
const sources = Object.fromEntries(inputs.sort().map((file) => [file, sha(readFileSync(join(root, file)))]));
const sourceDigest = sha(JSON.stringify(sources));
function lockVersion(name) {
  const lock = readFileSync(join(root, 'Cargo.lock'), 'utf8');
  const versions = [...lock.matchAll(/\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"/g)]
    .filter((match) => match[1] === name).map((match) => match[2]);
  if (versions.length !== 1) throw new Error(`expected exactly one ${name} package in Cargo.lock`);
  return versions[0];
}
const uniffi = lockVersion('uniffi');
const wasmBindgen = lockVersion('wasm-bindgen');
function copy(source, target) {
  mkdirSync(dirname(target), { recursive: true });
  cpSync(join(root, source), target, { recursive: true });
}
function manifest(target) {
  const artifacts = Object.fromEntries(files(target).filter((f) => !f.endsWith('/SOURCE.json'))
    .map((file) => [relative(target, file), sha(readFileSync(file))]));
  writeFileSync(join(target, 'SOURCE.json'), JSON.stringify({ schema: 1, crate: 'den-sync', version: '0.1.0',
    rust: '1.95.0', uniffi, wasmBindgen, sourceDigest, sources, artifacts }, null, 2) + '\n');
}
function canonicalPath(target) {
  let existing = resolve(target);
  const missing = [];
  while (true) {
    try {
      return resolve(realpathSync(existing), ...missing.reverse());
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      const parent = dirname(existing);
      if (parent === existing) throw error;
      missing.push(relative(parent, existing));
      existing = parent;
    }
  }
}
function overlaps(first, second) {
  const contains = (parent, child) => {
    const pathFromParent = relative(parent, child);
    return pathFromParent === '' || (pathFromParent !== '..'
      && !pathFromParent.startsWith(`..${sep}`) && !isAbsolute(pathFromParent));
  };
  return contains(first, second) || contains(second, first);
}
function preflightDestinations(native, web) {
  const source = { lexical: resolve(root), canonical: canonicalPath(root) };
  const destinations = [
    { name: 'native', lexical: resolve(native) },
    { name: 'web', lexical: resolve(web) },
  ].map((destination) => ({ ...destination, canonical: canonicalPath(destination.lexical) }));
  for (const destination of destinations) {
    if (overlaps(destination.lexical, source.lexical) || overlaps(destination.canonical, source.canonical)) {
      throw new Error(`refusing unsafe ${destination.name} vendor destination: ${destination.lexical}`);
    }
  }
  const [nativeDestination, webDestination] = destinations;
  if (overlaps(nativeDestination.lexical, webDestination.lexical)
      || overlaps(nativeDestination.canonical, webDestination.canonical)) {
    throw new Error('native and web vendor destinations must not overlap');
  }
  return { native: nativeDestination.lexical, web: webDestination.lexical };
}
export function vendor({
  native = process.env.DEN_NATIVE_VENDOR ?? join(root, '../den/Vendor/DenCore'),
  web = process.env.DEN_WEB_VENDOR ?? join(root, '../den-edge/web/src/vendor/den-core'),
} = {}) {
  const required = ['Swift/Package.swift', 'Swift/Sources', 'Swift/Artifacts', 'LICENSE',
    'web/index.js', 'web/index.d.ts', 'web/assistant.js', 'web/assistant.d.ts',
    ...['den_core', 'den_core_assistant'].flatMap((module) =>
      ['.js', '.d.ts', '_bg.wasm', '_bg.wasm.d.ts'].map((suffix) => `web/generated/${module}${suffix}`)),
    ...['policy-v1.json', 'policy-v3.json', 'profiles-v1.json']
      .map((fixture) => `crates/den-sync/tests/fixtures/${fixture}`)];
  for (const source of required) accessSync(join(root, source));

  ({ native, web } = preflightDestinations(native, web));
  rmSync(native, { recursive: true, force: true });
  for (const name of ['Package.swift', 'Sources', 'Artifacts']) copy(`Swift/${name}`, join(native, name));
  copy('LICENSE', join(native, 'LICENSE'));
  // The golden cases travel with the binary, as they already do for the web. Kept beside it rather than copied
  // into the client's own tests, so the manifest covers them and the two cannot drift apart unnoticed.
  for (const fixture of ['policy-v1.json', 'policy-v3.json', 'profiles-v1.json']) {
    copy(`crates/den-sync/tests/fixtures/${fixture}`, join(native, fixture));
  }
  manifest(native);

  rmSync(web, { recursive: true, force: true });
  for (const name of ['index.js', 'index.d.ts', 'assistant.js', 'assistant.d.ts']) copy(`web/${name}`, join(web, name));
  // Two modules: every op but the assistant's (`index.js`), and the assistant ops (`assistant.js`), loaded on demand.
  for (const module of ['den_core', 'den_core_assistant']) {
    for (const suffix of ['.js', '.d.ts', '_bg.wasm', '_bg.wasm.d.ts']) {
      copy(`web/generated/${module}${suffix}`, join(web, 'generated', `${module}${suffix}`));
    }
  }
  copy('LICENSE', join(web, 'LICENSE'));
  for (const fixture of ['policy-v1.json', 'policy-v3.json', 'profiles-v1.json']) {
    copy(`crates/den-sync/tests/fixtures/${fixture}`, join(web, fixture));
  }
  manifest(web);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) vendor();
