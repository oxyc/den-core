import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { evaluate, initialize } from '../web/index.js';
assert.throws(() => evaluate('{}'), /not initialized/);
await initialize(readFileSync(new URL('../web/generated/den_core_bg.wasm', import.meta.url)));
const fixture = JSON.parse(readFileSync(new URL('../crates/den-sync/tests/fixtures/policy-v1.json', import.meta.url)));
for (const test of fixture.cases) {
  const result = JSON.parse(evaluate(JSON.stringify(test.request)));
  assert.deepEqual(result, 'error' in test ? {version:1,error:test.error} : {version:1,ok:test.ok}, test.name);
}
console.log(`PASS: ${fixture.cases.length} shared contract cases through real WASM`);
