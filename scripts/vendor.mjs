// Mechanical packaging only. Clients consume immutable generated artifacts, never a sibling at runtime/build.
import { createHash } from 'node:crypto';
import { cpSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('..', import.meta.url));
const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
function files(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? files(join(dir, entry.name)) : [join(dir, entry.name)]).sort();
}
const inputs = ['Cargo.toml', 'Cargo.lock', ...files(join(root, 'crates')).map((f) => relative(root, f)),
  ...files(join(root, 'bindings')).map((f) => relative(root, f)),
  'scripts/build-bindings.sh', 'scripts/vendor.mjs',
  'Swift/Package.swift', 'web/index.js', 'web/index.d.ts'];
inputs.push('LICENSE');
const sources = Object.fromEntries(inputs.sort().map((file) => [file, sha(readFileSync(join(root, file)))]));
const sourceDigest = sha(JSON.stringify(sources));
function copy(source, target) {
  mkdirSync(dirname(target), { recursive: true });
  cpSync(join(root, source), target, { recursive: true });
}
function manifest(target) {
  const artifacts = Object.fromEntries(files(target).filter((f) => !f.endsWith('/SOURCE.json'))
    .map((file) => [relative(target, file), sha(readFileSync(file))]));
  writeFileSync(join(target, 'SOURCE.json'), JSON.stringify({ schema: 1, crate: 'den-sync', version: '0.1.0',
    rust: '1.95.0', uniffi: '0.29.5', wasmBindgen: '0.2.128', sourceDigest, sources, artifacts }, null, 2) + '\n');
}
const native = join(root, '../den/Vendor/DenCore');
for (const name of ['Package.swift', 'Sources', 'Artifacts']) copy(`Swift/${name}`, join(native, name));
copy('LICENSE', join(native, 'LICENSE'));
// The golden cases travel with the binary, as they already do for the web. Kept beside it rather than copied
// into the client's own tests, so the manifest covers them and the two cannot drift apart unnoticed.
copy('crates/den-sync/tests/fixtures/policy-v1.json', join(native, 'policy-v1.json'));
manifest(native);
const web = join(root, '../den-edge/web/src/vendor/den-core');
for (const name of ['index.js', 'index.d.ts']) copy(`web/${name}`, join(web, name));
for (const name of ['den_core.js', 'den_core.d.ts', 'den_core_bg.wasm', 'den_core_bg.wasm.d.ts']) {
  copy(`web/generated/${name}`, join(web, 'generated', name));
}
// Remove only the obsolete generated inline copy when updating a previously vendored package.
rmSync(join(web, 'generated/wasm-data.js'), { force: true });
copy('LICENSE', join(web, 'LICENSE'));
copy('crates/den-sync/tests/fixtures/policy-v1.json', join(web, 'policy-v1.json'));
manifest(web);
