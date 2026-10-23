import assert from 'node:assert/strict';
import { readFileSync, statSync } from 'node:fs';
import { gzipSync } from 'node:zlib';
import { evaluateStartup, initializeStartup } from '../web/startup.js';
import { evaluateDeferred, initializeDeferred } from '../web/deferred.js';
import { STARTUP_OPERATIONS, isStartupOperation } from '../web/lifecycle.js';

// Everything traced through a true uncached Home render and its immediate subscriptions: decode/merge incoming rows,
// validate controls, project Home, restore events, and answer download state without loading later policy.
const STARTUP_OPS = new Set(STARTUP_OPERATIONS);
assert.equal(STARTUP_OPS.size, STARTUP_OPERATIONS.length, 'startup operation manifest has no duplicates');
for (const op of STARTUP_OPERATIONS) assert.equal(isStartupOperation(op), true);
assert.equal(isStartupOperation('apply_write'), false);

await initializeStartup(readFileSync(new URL('../web/generated/den_core_startup_bg.wasm', import.meta.url)));
await initializeDeferred(readFileSync(new URL('../web/generated/den_core_deferred_bg.wasm', import.meta.url)));

const fixtures = ['policy-v1.json', 'policy-v4.json', 'profiles-v1.json'].map((name) =>
  JSON.parse(
    readFileSync(new URL(`../crates/den-sync/tests/fixtures/${name}`, import.meta.url)),
  ),
);
const approvalMerge = fixtures
  .flatMap((fixture) => fixture.cases)
  .find((test) => test.request.op === 'kid_title_approvals_merge');
assert.ok(approvalMerge, 'approval merge fixture supplies a valid approval document');
fixtures.push({
  cases: [
    {
      name: 'startup validates a stored Kid approval document',
      request: {
        op: 'kid_title_approvals_validate',
        logicalName: approvalMerge.request.logicalName,
        document: approvalMerge.request.a,
        directory: approvalMerge.request.directory,
      },
      ok: approvalMerge.request.a,
    },
    {
      name: 'startup projects an empty title document',
      request: {
        op: 'title_state',
        title: { format: 4, kind: 'title', title: { type: 'movie', id: 550 } },
        now: 1_000,
      },
      ok: {
        status: null,
        resume: null,
        reaction: null,
        deleted: null,
        dismissed: null,
        episodesReset: null,
        addedAt: null,
        watchedAt: null,
      },
    },
    {
      name: 'deferred reaches profile materialization validation',
      request: { op: 'profiles_materialize', document: {} },
      error: 'profiles_migration_required',
    },
    {
      name: 'deferred selects the runtime profile capability',
      request: {
        op: 'profiles_capability',
        input: {
          enrollment: 'off',
          wireMin: 5,
          directory: 'valid',
          request: 'ordinary',
          wire: 5,
          authenticated: true,
        },
      },
      ok: { state: 'runtime', effect: 'serve-runtime' },
    },
    {
      name: 'deferred derives a watch row name',
      request: { op: 'watch_name', media: 'tv', id: 1399, season: 1, episode: 1 },
      ok: 'wat:tv:1399:1:0',
    },
  ],
});
let count = 0;
let startupCount = 0;
let deferredCount = 0;
const coveredStartup = new Set();
for (const fixture of fixtures) {
  for (const test of fixture.cases) {
    if (test.request.op.startsWith('assistant_') || test.request.op === 'recovery_derive') continue;
    const startup = STARTUP_OPS.has(test.request.op);
    const request = JSON.stringify(test.request);
    const result = JSON.parse((startup ? evaluateStartup : evaluateDeferred)(request));
    assert.deepEqual(
      result,
      'error' in test ? { version: 1, error: test.error } : { version: 1, ok: test.ok },
      test.name,
    );
    assert.equal(
      JSON.parse((startup ? evaluateDeferred : evaluateStartup)(request)).error,
      'invalid_request',
      `${test.name}: opposite module refuses ${test.request.op}`,
    );
    count++;
    if (startup) {
      startupCount++;
      coveredStartup.add(test.request.op);
    }
    else deferredCount++;
  }
}

assert.deepEqual(
  [...coveredStartup].sort(),
  [...STARTUP_OPS].sort(),
  'every startup operation has a real-Wasm contract case',
);

console.log(
  `PASS: ${count} lifecycle contract cases (${startupCount} startup, ${deferredCount} deferred)`,
);
const budgets = new Map([
  ['den_core_startup', 420_000],
  ['den_core_deferred', 860_000],
]);
let combined = 0;
for (const [module, budget] of budgets) {
  const file = new URL(`../web/generated/${module}_bg.wasm`, import.meta.url);
  const bytes = statSync(file).size;
  combined += bytes;
  assert.ok(bytes <= budget, `${module}_bg.wasm is ${bytes} bytes (budget ${budget})`);
  const gzip = gzipSync(readFileSync(file), { level: 9 }).byteLength;
  console.log(`PASS: ${module}_bg.wasm ${bytes} raw, ${gzip} gzip (budget ${budget})`);
}
assert.ok(combined <= 1_260_000, `lifecycle modules total ${combined} bytes (budget 1260000)`);
console.log(`PASS: lifecycle modules total ${combined} raw (budget 1260000)`);
