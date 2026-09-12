import init, { evaluate as rustEvaluate } from './generated/den_core.js';

let initialized = false;
let pending;

/** Share concurrent callers, and allow a later attempt after a failed download or compilation. */
export function initialize(input) {
  if (initialized) return Promise.resolve();
  if (!pending) {
    const source = input ?? fetch(new URL('./generated/den_core_bg.wasm', import.meta.url), {
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

/** Policy stays synchronous after explicit asynchronous initialization. Never substitute empty state. */
export function evaluate(request) {
  if (!initialized) throw new Error('Sync core is not initialized');
  return rustEvaluate(request);
}
