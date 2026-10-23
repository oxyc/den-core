// Reproducible browser-WASM operation benchmark for issue #92.
// Build first with `bash scripts/build-bindings.sh wasm`, then run with Node 24:
//   node bench/season-projection.mjs
// This is a warmed microbenchmark plus first-journey evidence for one season, not a full-library or real-TV timing.

import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {performance} from 'node:perf_hooks';
import {fileURLToPath} from 'node:url';
import {evaluate, initialize} from '../web/index.js';

const NOW = 1_700_000_200_000;
const DEVICE = 'a1b2c3d4e5f60718';

function fixture(count) {
  const title = {
    format: 4,
    kind: 'title',
    title: {type: 'tv', id: 1399},
    episodesReset: [1_700_000_000_000, 0, DEVICE],
  };
  const season = {
    format: 4,
    kind: 'season',
    title: {type: 'tv', id: 1399},
    season: 1,
    seasonReset: null,
    episodes: {},
  };
  for (let episode = 1; episode <= count; episode++) {
    const at = 1_700_000_000_000 + episode * 1000;
    season.episodes[episode] = {
      progress: {value: episode % 3 === 0 ? 0.42 : 1, at: [at, 0, DEVICE], viewing: episode % 4},
      imported: false,
      plays: {0: at},
      cleared: null,
    };
  }
  return {title, season};
}

function requests(count) {
  const {title, season} = fixture(count);
  return {
    repeated: Array.from({length: count}, (_, index) =>
      JSON.stringify({op: 'episode_state_v4', title, season, episode: index + 1, now: NOW})),
    batch: [JSON.stringify({op: 'season_state_v4', title, season, now: NOW})],
  };
}

function journey(wires) {
  return wires.map(wire => evaluate(wire));
}

function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}

async function child(strategy, count) {
  const startInitialize = performance.now();
  await initialize(readFileSync(new URL('../web/generated/den_core_bg.wasm', import.meta.url)));
  const initializeMs = performance.now() - startInitialize;
  const wires = requests(count)[strategy];
  const startCold = performance.now();
  const coldAnswers = journey(wires);
  const coldMs = performance.now() - startCold;
  for (let index = 0; index < 3; index++) journey(wires);
  const samples = Array.from({length: 9}, () => {
    const start = performance.now();
    journey(wires);
    return performance.now() - start;
  });
  const answerBytes = coldAnswers.reduce((sum, answer) => sum + Buffer.byteLength(answer), 0);
  return {
    strategy,
    count,
    initializeMs,
    coldMs,
    warmMedianMs: median(samples),
    calls: wires.length,
    requestBytes: wires.reduce((sum, wire) => sum + Buffer.byteLength(wire), 0),
    answerBytes,
  };
}

if (process.argv[2] === '--child') {
  console.log(JSON.stringify(await child(process.argv[3], Number(process.argv[4]))));
} else {
  const self = fileURLToPath(import.meta.url);
  const results = [];
  for (const count of [24, 100]) {
    for (const strategy of ['repeated', 'batch']) {
      results.push(JSON.parse(execFileSync(process.execPath, [self, '--child', strategy, String(count)], {
        encoding: 'utf8',
      })));
    }
  }
  for (const count of [24, 100]) {
    const repeated = results.find(result => result.count === count && result.strategy === 'repeated');
    const batch = results.find(result => result.count === count && result.strategy === 'batch');
    assert.equal(batch.calls, 1);
    console.log(`\n${count} episodes (Node ${process.versions.node}, separate fresh processes)`);
    console.table([repeated, batch].map(({strategy, initializeMs, coldMs, warmMedianMs, calls,
      requestBytes, answerBytes}) => ({strategy, initializeMs, coldMs, warmMedianMs, calls,
      requestBytes, answerBytes, serializedBytes: requestBytes + answerBytes})));
  }
  console.log('\nCold is the first projection journey after initialization; warm is the median of nine journeys after three warmups.');
  console.log('Allocation/peak evidence: cargo test --locked -p den-sync --test season_projection_memory -- --nocapture');
  console.log('Scope: one synthetic season only; no full-library journey or real-device speedup is claimed.');
}
