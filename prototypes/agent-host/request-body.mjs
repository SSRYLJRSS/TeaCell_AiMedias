import { Buffer } from 'node:buffer';
import { setTimeout, clearTimeout } from 'node:timers';
import { setImmediate as yieldToEvents } from 'node:timers/promises';
import { performance } from 'node:perf_hooks';

// Bound ingestion before assembling the body. Request construction and the
// caller's already-created SDK JSON are outside this transport's allocation.
export async function readRequestBytes(request, signal, limit = 2 * 1024 * 1024) {
  if (signal.aborted) throw signal.reason;
  if (!request.body) return Buffer.alloc(0);
  const reader = request.body.getReader();
  let interruptRead;
  let interruption;
  const stop = error => {
    interruption ??= error;
    interruptRead?.(interruption);
    // A hostile producer's cancel promise must not delay returning the error.
    reader.cancel(error).catch(() => {});
  };
  const abort = () => stop(signal.reason);
  signal.addEventListener('abort', abort, { once: true });
  const timeout = setTimeout(() => stop(new Error('BODY_READ_TIMEOUT')), 8000);
  const deadline = performance.now() + 8000;
  let storage = Buffer.alloc(0);
  let size = 0;
  let reads = 0;
  try {
    if (signal.aborted) abort();
    while (true) {
      if (performance.now() >= deadline) throw new Error('BODY_READ_TIMEOUT');
      // Synchronous producers can otherwise starve timers and cancellation.
      if (++reads % 128 === 0) await yieldToEvents();
      if (interruption) throw interruption;
      // A new interrupt promise for only this read. Repeatedly racing against
      // one pending promise retains a reaction for every previous chunk.
      const interrupted = new Promise((_resolve, reject) => { interruptRead = reject; });
      let result;
      try { result = await Promise.race([reader.read(), interrupted]); }
      finally { interruptRead = undefined; }
      const { done, value } = result;
      if (signal.aborted) throw signal.reason;
      if (performance.now() >= deadline) throw new Error('BODY_READ_TIMEOUT');
      if (done) return storage.subarray(0, size);
      if (!(value instanceof Uint8Array)) throw new Error('INVALID_BODY');
      const nextSize = size + value.byteLength;
      if (nextSize > limit) throw new Error('BODY_LIMIT');
      if (nextSize > storage.length) {
        const expanded = Buffer.allocUnsafe(Math.min(limit, Math.max(16384, nextSize, storage.length * 2)));
        storage.copy(expanded, 0, 0, size);
        storage = expanded;
      }
      // One growing byte buffer, no unbounded per-chunk object list. Empty
      // chunks allocate nothing; unused capacity never enters the request.
      storage.set(value, size);
      size = nextSize;
    }
  } catch (error) {
    reader.cancel(error).catch(() => {});
    throw error;
  } finally {
    clearTimeout(timeout);
    signal.removeEventListener('abort', abort);
    reader.releaseLock();
  }
}
