import init, { evaluate_assistant as rustEvaluate } from './generated/den_core_assistant.js';

let initialized = false;
let pending;

/**
 * The assistant and playback-receipt crypto module, apart from `index.js` so its crypto loads only where it is used.
 * Shares concurrent callers, and allows a later attempt after a failed download or compilation.
 */
export function initializeAssistant(input) {
  if (initialized) return Promise.resolve();
  if (!pending) {
    const source = input ?? fetch(new URL('./generated/den_core_assistant_bg.wasm', import.meta.url), {
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

/** The `assistant_*` and `playback_receipt_*` ops, synchronous after initialization. Never substitute empty state. */
export function evaluateAssistant(request) {
  if (!initialized) throw new Error('Assistant core is not initialized');
  return rustEvaluate(request);
}
