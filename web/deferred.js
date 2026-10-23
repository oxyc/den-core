import init, { evaluate_deferred as rustEvaluate } from './generated/den_core_deferred.js';

let initialized = false;
let pending;

export function initializeDeferred(input) {
  if (initialized) return Promise.resolve();
  if (!pending) {
    const source = input ?? fetch(new URL('./generated/den_core_deferred_bg.wasm', import.meta.url), {
      signal: AbortSignal.timeout(15000),
    });
    pending = init({ module_or_path: source }).then(() => {
      initialized = true;
    }).catch((error) => {
      pending = undefined;
      throw error;
    });
  }
  return pending;
}

export function evaluateDeferred(request) {
  if (!initialized) throw new Error('Deferred core is not initialized');
  return rustEvaluate(request);
}
