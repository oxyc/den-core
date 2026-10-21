import init, { evaluate_recovery_kdf as rustEvaluate } from './generated/den_core_recovery.js';

let initialized = false;
let pending;

/** Load recovery code §3's memory-hard KDF only while making or redeeming a code. */
export function initializeRecovery(input) {
  if (initialized) return Promise.resolve();
  if (!pending) {
    const source = input ?? fetch(new URL('./generated/den_core_recovery_bg.wasm', import.meta.url), {
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

/** `recovery_derive`, synchronous after initialization. Never substitute an empty key. */
export function evaluateRecovery(request) {
  if (!initialized) throw new Error('Recovery core is not initialized');
  return rustEvaluate(request);
}
