import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { evaluate, initialize } from '../web/index.js';
import { evaluateAssistant, initializeAssistant } from '../web/assistant.js';
assert.throws(() => evaluate('{}'), /not initialized/);
assert.throws(() => evaluateAssistant('{}'), /not initialized/);
await initialize(readFileSync(new URL('../web/generated/den_core_bg.wasm', import.meta.url)));
await initializeAssistant(readFileSync(new URL('../web/generated/den_core_assistant_bg.wasm', import.meta.url)));
const fixtures = ['policy-v1.json', 'policy-v3.json', 'policy-v4.json', 'profiles-v1.json'].map(name =>
  JSON.parse(readFileSync(new URL(`../crates/den-sync/tests/fixtures/${name}`, import.meta.url))));
// The assistant ops run in their own module, and each module refuses the other's ops.
const assistant = (request) => typeof request.op === 'string' && request.op.startsWith('assistant_');
let count = 0;
let assistantCount = 0;
for (const fixture of fixtures) {
  for (const test of fixture.cases) {
    const own = assistant(test.request) ? evaluateAssistant : evaluate;
    const result = JSON.parse(own(JSON.stringify(test.request)));
    assert.deepEqual(result, 'error' in test ? {version:1,error:test.error} : {version:1,ok:test.ok}, test.name);
    if (assistant(test.request)) {
      assert.deepEqual(JSON.parse(evaluate(JSON.stringify(test.request))), {version: 1, error: 'assistant_module'},
        test.name);
      assistantCount++;
    } else {
      assert.equal(JSON.parse(evaluateAssistant(JSON.stringify(test.request))).error, 'invalid_request', test.name);
    }
    count++;
  }
}
const profileFixture = fixtures.at(-1);
for (const guard of profileFixture.partitionIdentityGuards) {
  for (const status of ['newer', 'future']) {
    const opened = status === 'newer'
      ? {status: 'newer', reason: 'framing', name: guard.identity}
      : {status: 'row', name: guard.identity, legacy: false, row: {kind: 'future', schema: 1}};
    const request = {op: 'profiles_migration_partition_rows', input: {
      base: 1,
      ownerPolicyAt: [1, 0, 'a1b2c3d4e5f60718'],
      rows: [{k: 'a'.repeat(64), seq: 1, v: 'sealed', opened}],
    }};
    assert.deepEqual(JSON.parse(evaluate(JSON.stringify(request))),
      {version: 1, error: guard.error}, `${guard.name} ${status}`);
    count++;
  }
}
assert.ok(assistantCount > 0);
console.log(`PASS: ${count} shared contract cases through real WASM (${assistantCount} in the assistant module)`);
