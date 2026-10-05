// One recovery_derive at a time, in a Worker as Den Web runs it: the wasm build `scripts/build-bindings.sh` makes, with
// no SIMD. Reports each run's time and the Worker's wasm memory before and after, since a phone may fail to grow it.
import init, { evaluate } from '../web/generated/den_core.js';

const DATA = 'GEB2LP9UC63WQ95UNSLTXM';

self.onmessage = async (event) => {
  const runs = event.data.runs;
  try {
    const exports = await init({ module_or_path: new URL('../web/generated/den_core_bg.wasm', import.meta.url) });
    const before = exports.memory.buffer.byteLength;
    const times = [];
    for (let i = 0; i < runs; i++) {
      const start = performance.now();
      const out = JSON.parse(evaluate(JSON.stringify({ op: 'recovery_derive', data: DATA })));
      times.push(performance.now() - start);
      if (!out.ok) throw new Error(out.error ?? 'no answer');
    }
    self.postMessage({ times, memoryBefore: before, memoryAfter: exports.memory.buffer.byteLength });
  } catch (error) {
    self.postMessage({ error: String(error) });
  }
};
