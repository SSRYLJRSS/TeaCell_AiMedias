import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { setImmediate } from 'node:timers';
import { createWriteQueue } from '../src/write-queue.mjs';

test('frame writes stay ordered and pause behind writable backpressure', async () => {
  class SlowWritable extends EventEmitter {
    chunks = [];

    write(chunk) {
      this.chunks.push(chunk);
      return chunk !== 'second';
    }
  }

  const stream = new SlowWritable();
  const output = createWriteQueue(stream);
  const first = output.write('first');
  const second = output.write('second');
  const third = output.write('third');

  await first;
  await new Promise(resolveNextTick => setImmediate(resolveNextTick));
  assert.deepEqual(stream.chunks, ['first', 'second']);

  stream.emit('drain');
  await Promise.all([second, third]);
  assert.deepEqual(stream.chunks, ['first', 'second', 'third']);
});

test('write errors reject the queue and do not write later frames', async () => {
  class BrokenWritable extends EventEmitter {
    write() {
      throw new Error('PIPE_CLOSED');
    }
  }

  const output = createWriteQueue(new BrokenWritable());
  await assert.rejects(output.write('first'), { message: 'PIPE_CLOSED' });
  await assert.rejects(output.write('second'), { message: 'PIPE_CLOSED' });
});
