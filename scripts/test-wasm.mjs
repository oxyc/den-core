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
assert.ok(assistantCount > 0);
console.log(`PASS: ${count} shared contract cases through real WASM (${assistantCount} in the assistant module)`);
