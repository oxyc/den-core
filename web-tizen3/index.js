import coreUrl from './generated/den_core.js?url';

let initialized = false;
let pending;
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });

function core() {
  const value = globalThis.DenTizen3Core;
  if (!value) throw new Error('Tizen 3 sync core is not initialized');
  return value;
}

/** Keep the JavaScript fallback out of Babel and app/Worker bundles; load it once as a classic asset. */
export function initialize() {
  if (initialized) return Promise.resolve();
  if (globalThis.DenTizen3Core) {
    initialized = true;
    return Promise.resolve();
  }
  if (!pending) {
    pending = new Promise((resolve, reject) => {
      if (typeof importScripts === 'function') {
        try {
          importScripts(coreUrl);
          core();
          initialized = true;
          resolve();
        } catch (error) {
          reject(error);
        }
        return;
      }
      const script = document.createElement('script');
      script.src = coreUrl;
      script.onload = () => {
        try {
          core();
          initialized = true;
          resolve();
        } catch (error) {
          reject(error);
        }
      };
      script.onerror = () => reject(new Error('Tizen 3 sync core could not be loaded'));
      document.head.append(script);
    }).catch((error) => {
      pending = undefined;
      throw error;
    });
  }
  return pending;
}

export function evaluate(request) {
  if (!initialized) throw new Error('Sync core is not initialized');
  const { memory, den_alloc: allocate, den_free: free, den_evaluate: evaluateRaw } = core();
  const input = encoder.encode(request);
  const inputPointer = allocate(input.byteLength);
  const outputSlot = allocate(8);
  try {
    new Uint8Array(memory.buffer, inputPointer, input.byteLength).set(input);
    evaluateRaw(inputPointer, input.byteLength, outputSlot);
    const result = new Uint32Array(memory.buffer, outputSlot, 2);
    const outputPointer = result[0];
    const outputLength = result[1];
    try {
      return decoder.decode(new Uint8Array(memory.buffer, outputPointer, outputLength));
    } finally {
      free(outputPointer, outputLength);
    }
  } finally {
    free(inputPointer, input.byteLength);
    free(outputSlot, 8);
  }
}
