import { initSync, evaluate as rustEvaluate } from './generated/den_core.js';
import wasmBase64 from './generated/wasm-data.js';

let initialized = false;

/** Synchronous after a single local instantiation: no CDN, credential access, or network fallback. */
export function evaluate(request) {
  if (!initialized) {
    initSync({ module: Uint8Array.from(atob(wasmBase64), (c) => c.charCodeAt(0)) });
    initialized = true;
  }
  return rustEvaluate(request);
}
