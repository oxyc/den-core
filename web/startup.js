import init, { evaluate_startup as rustEvaluate } from './generated/den_core_startup.js';

let initialized = false;
let pending;

export function initializeStartup(input) {
  if (initialized) return Promise.resolve();
  if (!pending) {
    const source = input ?? fetch(new URL('./generated/den_core_startup_bg.wasm', import.meta.url), {
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

export function evaluateStartup(request) {
  if (!initialized) throw new Error('Startup core is not initialized');
  return rustEvaluate(request);
}
