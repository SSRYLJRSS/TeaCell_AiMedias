import test from 'node:test';
import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { FrameDecoder, encodeFrame, MAX_FRAME_BYTES } from '../src/protocol.mjs';

test('split UTF-8 and adjacent frames preserve exact content', () => {
  const bytes = Buffer.from(encodeFrame({ text: '茶馆🙂' }) + encodeFrame({ id: 2 }));
  const decoder = new FrameDecoder();
  const frames = [];
  for (const byte of bytes) frames.push(...decoder.push(Buffer.from([byte])));
  decoder.end();
  assert.deepEqual(frames, [{ text: '茶馆🙂' }, { id: 2 }]);
});

test('frame decoder rejects oversized, malformed, and truncated input', () => {
  assert.throws(() => new FrameDecoder().push(Buffer.alloc(MAX_FRAME_BYTES + 1, 65)), /FRAME_TOO_LARGE/);
  for (const text of ['[]\n', 'null\n', '42\n', '\n', 'log pollution\n']) {
    assert.throws(() => new FrameDecoder().push(Buffer.from(text)));
  }
  const decoder = new FrameDecoder();
  decoder.push(Buffer.from('{'));
  assert.throws(() => decoder.end(), /TRUNCATED_FRAME/);
});

test('frame decoder rejects malformed UTF-8', () => {
  const bytes = Buffer.concat([Buffer.from('{"text":"'), Buffer.from([0xc3, 0x28]), Buffer.from('"}\n')]);
  assert.throws(() => new FrameDecoder().push(bytes));
});
