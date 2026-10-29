import assert from 'node:assert/strict';
import { readFileSync, statSync } from 'node:fs';
import { gzipSync } from 'node:zlib';
import { evaluate, initialize } from '../web/index.js';
import { evaluateAssistant, initializeAssistant } from '../web/assistant.js';
import { evaluateRecovery, initializeRecovery } from '../web/recovery.js';
assert.throws(() => evaluate('{}'), /not initialized/);
assert.throws(() => evaluateAssistant('{}'), /not initialized/);
assert.throws(() => evaluateRecovery('{}'), /not initialized/);
await initialize(readFileSync(new URL('../web/generated/den_core_bg.wasm', import.meta.url)));
await initializeAssistant(readFileSync(new URL('../web/generated/den_core_assistant_bg.wasm', import.meta.url)));
await initializeRecovery(readFileSync(new URL('../web/generated/den_core_recovery_bg.wasm', import.meta.url)));
const fixtures = ['policy-v1.json', 'policy-v4.json', 'profiles-v1.json'].map(name =>
  JSON.parse(readFileSync(new URL(`../crates/den-sync/tests/fixtures/${name}`, import.meta.url))));
// Optional operations run in their own modules, and every module refuses operations outside its capability.
const assistant = (request) => typeof request.op === 'string' && request.op.startsWith('assistant_');
const recovery = (request) => request.op === 'recovery_derive';
let count = 0;
let assistantCount = 0;
let recoveryCount = 0;
for (const fixture of fixtures) {
  for (const test of fixture.cases) {
    const own = assistant(test.request)
      ? evaluateAssistant
      : recovery(test.request)
        ? evaluateRecovery
        : evaluate;
    const result = JSON.parse(own(JSON.stringify(test.request)));
    assert.deepEqual(result, 'error' in test ? {version:1,error:test.error} : {version:1,ok:test.ok}, test.name);
    if (assistant(test.request)) {
      assert.deepEqual(JSON.parse(evaluate(JSON.stringify(test.request))), {version: 1, error: 'assistant_module'},
        test.name);
      assert.equal(JSON.parse(evaluateRecovery(JSON.stringify(test.request))).error, 'invalid_request', test.name);
      assistantCount++;
    } else if (recovery(test.request)) {
      assert.equal(JSON.parse(evaluate(JSON.stringify(test.request))).error, 'invalid_request', test.name);
      assert.equal(JSON.parse(evaluateAssistant(JSON.stringify(test.request))).error, 'invalid_request', test.name);
      recoveryCount++;
    } else {
      assert.equal(JSON.parse(evaluateAssistant(JSON.stringify(test.request))).error, 'invalid_request', test.name);
      assert.equal(JSON.parse(evaluateRecovery(JSON.stringify(test.request))).error, 'invalid_request', test.name);
    }
    count++;
  }
}
const profileFixture = fixtures.at(-1);

const clone = value => structuredClone(value);
const mutate = (document, mutation) => {
  const result = clone(document);
  const parts = mutation.path.split('/').slice(1);
  const key = parts.pop();
  let parent = result;
  for (const part of parts) parent = parent[part];
  if ('rename' in mutation) {
    parent[mutation.rename] = parent[key];
    delete parent[key];
  } else {
    parent[key] = clone(mutation.value);
  }
  return result;
};
for (const case_ of profileFixture.initialDirectoryConstructorCases) {
  let input = clone(profileFixture.initialDirectoryConstructorInput);
  for (const mutation of case_.mutations ?? []) input = mutate(input, mutation);
  if (case_.remove) {
    const parts = case_.remove.split('/').slice(1);
    const key = parts.pop();
    let parent = input;
    for (const part of parts) parent = parent[part];
    delete parent[key];
  }
  // The JSON boundary uses JCS, where ECMAScript serializes negative zero as `0`; normalize the fixture through
  // the same JSON representation before comparing rather than relying on JavaScript's observable `-0` value.
  const expected = 'error' in case_ ? {version: 1, error: case_.error} :
    {version: 1, ok: JSON.parse(JSON.stringify(case_.expect))};
  assert.deepEqual(JSON.parse(evaluate(JSON.stringify({op: 'profiles_construct_directory', input}))),
    expected, case_.name);
  count++;
}

// The generic WASM transport exposes the same Kid creation operations and keeps
// serde's recursion guard at the transport boundary.
const materialize = JSON.parse(evaluate(JSON.stringify({
  op: 'profiles_materialize', document: profileFixture.migrationBase,
})));
assert.deepEqual(materialize.ok.directory, profileFixture.migrationBase);
assert.ok(materialize.ok.jcs.length > 0);
assert.ok(materialize.ok.plaintext.length > 0);
count++;
const createJournal = {
  version: 1,
  profile: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  key: 'VVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVU=',
  display: {value: {name: 'Sam', avatar: 'fox'}, at: [1800000005000, 0, 'a1b2c3d4e5f60718']},
  libraryAt: [1800000005000, 1, 'a1b2c3d4e5f60718'],
  maximumAge: 8,
  policyAt: [1800000005000, 2, 'a1b2c3d4e5f60718'],
  deletedAt: [1800000005000, 3, 'a1b2c3d4e5f60718'],
};
for (const [input, effect] of [
  [{state: 'idle', explicitIntent: false}, 'no-op'],
  [{state: 'idle', explicitIntent: true, journal: createJournal}, 'persist-journal'],
]) {
  const response = JSON.parse(evaluate(JSON.stringify({op: 'profiles_create_next', input})));
  assert.equal(response.ok.effect, effect);
  count++;
}
const deepWire = '{"op":"profiles_materialize","document":{"migration":{},"futureNested":' +
  '{"next":'.repeat(1500) + 'null' + '}'.repeat(1500) + '}}';
assert.deepEqual(JSON.parse(evaluate(deepWire)), {version: 1, error: 'invalid_request'});
count++;
assert.ok(assistantCount > 0);
assert.ok(recoveryCount > 0);
console.log(`PASS: ${count} shared contract cases through real WASM (${assistantCount} assistant, ${recoveryCount} recovery)`);

// Every Worker that loads a module compiles its own copy, and in WebKit the machine code is most of what a copy
// costs (Den Web runs two library Workers). Keep enough headroom for toolchain noise without giving the compact
// request boundary's savings back.
for (const [module, budget] of [
  ['den_core', 920_000],
  ['den_core_assistant', 520_000],
  ['den_core_recovery', 140_000],
]) {
  const bytes = statSync(new URL(`../web/generated/${module}_bg.wasm`, import.meta.url)).size;
  assert.ok(bytes <= budget, `${module}_bg.wasm is ${bytes} bytes, over its ${budget}-byte budget`);
  console.log(`PASS: ${module}_bg.wasm ${bytes} bytes (budget ${budget})`);
}

const tizen3 = readFileSync(new URL('../web-tizen3/generated/den_core.js', import.meta.url));
const tizen3Gzip = gzipSync(tizen3, {level: 9}).byteLength;
assert.ok(tizen3.byteLength <= 2_050_000, `den_core.js is ${tizen3.byteLength} bytes, over its 2050000-byte budget`);
assert.ok(tizen3Gzip <= 590_000, `den_core.js gzip is ${tizen3Gzip} bytes, over its 590000-byte budget`);
console.log(`PASS: Tizen 3 core ${tizen3.byteLength} bytes, ${tizen3Gzip} gzip`);

// Exercise the pointer-only wasm2js transport too: the generic browser and native contracts use the same den-sync
// implementation, but a Tizen 3 packaging regression must not silently omit the new season operation.
await import('../web-tizen3/generated/den_core.js');
const tizenEvaluate = request => {
  const core = globalThis.DenTizen3Core;
  const input = new TextEncoder().encode(request);
  const inputPointer = core.den_alloc(input.byteLength);
  const outputSlot = core.den_alloc(8);
  try {
    new Uint8Array(core.memory.buffer, inputPointer, input.byteLength).set(input);
    core.den_evaluate(inputPointer, input.byteLength, outputSlot);
    const [outputPointer, outputLength] = new Uint32Array(core.memory.buffer, outputSlot, 2);
    try {
      return new TextDecoder('utf-8', {fatal: true})
        .decode(new Uint8Array(core.memory.buffer, outputPointer, outputLength));
    } finally {
      core.den_free(outputPointer, outputLength);
    }
  } finally {
    core.den_free(inputPointer, input.byteLength);
    core.den_free(outputSlot, 8);
  }
};
const seasonProjection = fixtures[1].cases.find(test =>
  test.name === 'one season projection preserves every single-episode state');
assert.ok(seasonProjection);
const seasonWire = JSON.stringify(seasonProjection.request);
assert.deepEqual(JSON.parse(tizenEvaluate(seasonWire)), JSON.parse(evaluate(seasonWire)));
console.log('PASS: season_state_v4 parity through the Tizen 3 core');
