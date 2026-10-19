import assert from 'node:assert/strict';
import { readFileSync, statSync } from 'node:fs';
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
for (const case_ of profileFixture.migrationDirectoryConstructorCases) {
  let input = clone(profileFixture.migrationDirectoryConstructorInput);
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
  // The neutral name answers exactly as the migration-named one.
  for (const op of ['profiles_migration_construct_directory', 'profiles_construct_directory']) {
    assert.deepEqual(JSON.parse(evaluate(JSON.stringify({op, input}))), expected, `${op}: ${case_.name}`);
    count++;
  }
}
for (const [evidence, expected] of [
  [null, profileFixture.migrationAuthorization.prepared],
  [profileFixture.migrationAuthorization.evidence, profileFixture.migrationAuthorization.bound],
]) {
  const request = {op: 'profiles_migration_authorization', input: {
    intent: profileFixture.migrationAuthorization.intent, evidence,
  }};
  assert.deepEqual(JSON.parse(evaluate(JSON.stringify(request))), {version: 1, ok: expected});
  count++;
}
const migrationInput = case_ => {
  const input = clone(case_);
  if (case_.observed === 'committed' && !case_.omitDirectory) {
    input.directory = clone(profileFixture.migrationBase);
    if (case_.directoryMutation) input.directory = mutate(input.directory, case_.directoryMutation);
  }
  if (case_.observed === 'legacy' && case_.state !== 'idle') {
    input.fence = clone(profileFixture.migrationFence);
    if (!case_.omitObservedFence) input.observedFence = clone(profileFixture.migrationFence);
    if (case_.fenceMutation) input.fence = mutate(input.fence, case_.fenceMutation);
    if (case_.observedFenceMutation) {
      input.observedFence = mutate(input.observedFence, case_.observedFenceMutation);
    }
  }
  const needsAuthorization = (case_.state === 'idle' && case_.explicitIntent) ||
    case_.state === 'commit-unknown' ||
    (case_.observed === 'legacy' && !['idle', 'restore-required'].includes(case_.state));
  if (needsAuthorization) {
    const authorization = profileFixture.migrationAuthorization;
    const intent = clone(authorization.intent);
    input.attempt = intent.attempt;
    input.householdId = intent.householdId;
    if (case_.state === 'idle') input.observedFence = clone(intent.source);
    if (case_.state === 'awaiting-confirmation') {
      input.backupDigest ??= authorization.evidence.backupDigest;
      input.stagedDigest ??= authorization.evidence.stagedDigest;
      input.wireMin ??= 5;
    }
    if (case_.commitAuthorized) {
      const evidence = clone(authorization.evidence);
      if (case_.state === 'commit-unknown') {
        input.fence ??= clone(intent.source);
        input.backupVerified ??= true;
        input.candidateKey ??= evidence.candidateKey;
        input.candidateHead ??= evidence.candidateHead;
        input.ownerVerified ??= true;
      }
      for (const key of ['candidateKey', 'candidateHead', 'backupDigest', 'stagedDigest', 'wireMin']) {
        if (key in input) evidence[key] = input[key];
        else if (['backupDigest', 'stagedDigest', 'wireMin'].includes(key)) input[key] = evidence[key];
      }
      evidence.fence = clone(input.fence ?? intent.source);
      const answer = JSON.parse(evaluate(JSON.stringify({
        op: 'profiles_migration_authorization', input: {intent, evidence},
      })));
      input.splitAuthorization = answer.ok ?? clone(authorization.prepared);
    } else {
      input.splitAuthorization = clone(authorization.prepared);
    }
    if (case_.splitAuthorizationMutation) {
      input.splitAuthorization = mutate(input.splitAuthorization, case_.splitAuthorizationMutation);
    }
    if (case_.omitSplitAuthorization) delete input.splitAuthorization;
    if (case_.plannerAttempt) input.attempt = case_.plannerAttempt;
  }
  if (case_.omitState) delete input.state;
  if (case_.omitObserved) delete input.observed;
  return input;
};
for (const case_ of profileFixture.migrationCases) {
  const request = {op: 'profiles_migration_next', input: migrationInput(case_)};
  const expected = 'error' in case_ ? {version: 1, error: case_.error} : {version: 1, ok: case_.expect};
  assert.deepEqual(JSON.parse(evaluate(JSON.stringify(request))), expected, case_.name);
  count++;
}
for (const case_ of [
  ...profileFixture.profileCapabilityContract.cases,
  ...profileFixture.profileCapabilityContract.mixedHost,
]) {
  const request = {op: 'profiles_capability', input: case_.input};
  const expected = 'error' in case_ ? {version: 1, error: case_.error} : {version: 1, ok: case_.expect};
  assert.deepEqual(JSON.parse(evaluate(JSON.stringify(request))), expected, case_.name ?? case_.household);
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
console.log(`PASS: ${count} shared contract cases through real WASM (${assistantCount} in the assistant module)`);

// Every Worker that loads a module compiles its own copy, and in WebKit the machine code is most of what a copy
// costs (Den Web runs two library Workers). Budgets with some headroom over the built size, below the 1.71 MB and
// 596 KB of the modules before they were built for size.
for (const [module, budget] of [['den_core', 1_450_000], ['den_core_assistant', 560_000]]) {
  const bytes = statSync(new URL(`../web/generated/${module}_bg.wasm`, import.meta.url)).size;
  assert.ok(bytes <= budget, `${module}_bg.wasm is ${bytes} bytes, over its ${budget}-byte budget`);
  console.log(`PASS: ${module}_bg.wasm ${bytes} bytes (budget ${budget})`);
}
