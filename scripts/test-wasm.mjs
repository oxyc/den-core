import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { evaluate, initialize } from '../web/index.js';
assert.throws(() => evaluate('{}'), /not initialized/);
await initialize(readFileSync(new URL('../web/generated/den_core_bg.wasm', import.meta.url)));
const fixtures = ['policy-v1.json', 'policy-v3.json', 'policy-v4.json'].map(name =>
  JSON.parse(readFileSync(new URL(`../crates/den-sync/tests/fixtures/${name}`, import.meta.url))));
let count = 0;
for (const fixture of fixtures) {
  for (const test of fixture.cases) {
    const result = JSON.parse(evaluate(JSON.stringify(test.request)));
    assert.deepEqual(result, 'error' in test ? {version:1,error:test.error} : {version:1,ok:test.ok}, test.name);
    count++;
  }
}
console.log(`PASS: ${count} shared contract cases through real WASM`);
