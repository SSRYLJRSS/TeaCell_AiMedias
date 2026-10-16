import process from 'node:process';
import { setTimeout } from 'node:timers';
import { readRequestBytes } from '../request-body.mjs';

const size = Number(process.argv[2]);
if (![0, 1].includes(size) || typeof globalThis.gc !== 'function') throw new Error('INVALID_MEMORY_FIXTURE');
globalThis.gc();
const baseline = process.memoryUsage().heapUsed;
let live;
let timerRan = false;
let reads = 0;
setTimeout(() => { timerRan = true; }, 0);
const body = new globalThis.ReadableStream({
  pull(controller) {
    if (++reads <= 200000) controller.enqueue(new Uint8Array(size));
    else {
      globalThis.gc();
      live = process.memoryUsage().heapUsed - baseline;
      controller.close();
    }
  },
}, { highWaterMark: 0 });
const request = new globalThis.Request('https://fixture.invalid/', { method: 'POST', body, duplex: 'half' });
const bytes = await readRequestBytes(request, new globalThis.AbortController().signal);
process.stdout.write(JSON.stringify({ bytes: bytes.length, live, timerRan, reads }));
