// Build den-sync's pointer-only wasm binding into the classic JavaScript core Chromium 47 can execute.
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const wasm2js = resolve(root, '.tools/bin/wasm2js');
const wasmOpt = resolve(root, '.tools/bin/wasm-opt');
const terser = resolve(root, 'node_modules/.bin/terser');
for (const tool of [wasm2js, wasmOpt, terser]) {
  if (!existsSync(tool)) throw new Error(`required pinned tool is missing: ${tool}`);
}
for (const [tool, prefix] of [
  [wasm2js, 'wasm2js version 133 (version_133)'],
  [wasmOpt, 'wasm-opt version 133 (version_133)'],
]) {
  const version = execFileSync(tool, ['--version'], { encoding: 'utf8' }).trim();
  if (version !== prefix) throw new Error(`expected ${prefix}, got ${version}`);
}

const target = resolve(root, 'target/tizen3-core');
const sysroot = execFileSync('rustc', ['--print', 'sysroot'], { encoding: 'utf8' }).trim();
const host = /^host: (.+)$/m.exec(execFileSync('rustc', ['-vV'], { encoding: 'utf8' }))?.[1];
const rustLld = process.env.RUST_LLD ?? (host && join(sysroot, 'lib/rustlib', host, 'bin/rust-lld'));
const encodedFlags = [
  '-C',
  'target-feature=-reference-types,-multivalue',
  `--remap-path-prefix=${root}=/den-core`,
  `--remap-path-prefix=${process.env.CARGO_HOME ?? join(homedir(), '.cargo')}=/cargo`,
  `--remap-path-prefix=${sysroot}=/rust-toolchain`,
].join('\x1f');
const features = (process.env.DEN_TIZEN3_FEATURES ?? '')
  .split(',')
  .map((feature) => feature.trim())
  .filter(Boolean);
const cargoArgs = [
  'build',
  '--locked',
  '--release',
  '-p',
  'den-core-web-tizen3',
  '--target',
  'wasm32-unknown-unknown',
  ...(features.length ? ['--features', features.join(',')] : []),
];
execFileSync(
  'cargo',
  cargoArgs,
  {
    cwd: root,
    stdio: 'inherit',
    env: {
      ...process.env,
      RUSTC_BOOTSTRAP: '1',
      RUSTFLAGS: '-C target-feature=-reference-types,-multivalue',
      CARGO_ENCODED_RUSTFLAGS: encodedFlags,
      CARGO_TARGET_DIR: target,
      ...(rustLld && existsSync(rustLld)
        ? { CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER: rustLld }
        : {}),
    },
  },
);

const wasm = join(target, 'wasm32-unknown-unknown/release/den_core_web_tizen3.wasm');
const optimized = join(target, 'den_core.optimized.wasm');
const converted = join(target, 'den_core.converted.js');
const generated = resolve(root, 'web-tizen3/generated/den_core.js');
mkdirSync(dirname(generated), { recursive: true });
execFileSync(wasmOpt, [wasm, '-O3', '-o', optimized], { stdio: 'inherit' });
execFileSync(wasm2js, [optimized, '-o', converted], { stdio: 'inherit' });
execFileSync(terser, [converted, '--compress', '--mangle', '--ecma', '5', '--output', generated], {
  stdio: 'inherit',
});
const moduleTail =
  'export var memory=retasmFunc.memory;export var den_alloc=retasmFunc.den_alloc;export var den_evaluate=retasmFunc.den_evaluate;export var den_free=retasmFunc.den_free;export var __data_end=retasmFunc.__data_end;export var __heap_base=retasmFunc.__heap_base;';
const globalTail =
  ';(typeof self!=="undefined"?self:globalThis).DenTizen3Core={memory:retasmFunc.memory,den_alloc:retasmFunc.den_alloc,den_evaluate:retasmFunc.den_evaluate,den_free:retasmFunc.den_free};';
const code = readFileSync(generated, 'utf8');
if (!code.endsWith(moduleTail))
  throw new Error('wasm2js/Terser export seam changed; refusing an unreviewed legacy core');
writeFileSync(generated, code.slice(0, -moduleTail.length) + globalTail);
console.log(`Tizen 3 JavaScript core generated (${readFileSync(generated).byteLength} bytes)`);
