import test from 'node:test';
import { Buffer } from 'node:buffer';
import assert from 'node:assert/strict';
import { FrameDecoder, encodeFrame, MAX_FRAME_BYTES } from '../protocol.mjs';

test('split UTF-8 and multiple frames preserve exact content', () => {
  const bytes = Buffer.from(encodeFrame({ text: '茶馆🙂' }) + encodeFrame({ id: 2 }));
  const decoder = new FrameDecoder();
  const result = [];
  for (const byte of bytes) result.push(...decoder.push(Buffer.from([byte])));
  decoder.end();
  assert.deepEqual(result, [{ text: '茶馆🙂' }, { id: 2 }]);
});

test('bounded parser rejects flooding without a delimiter', () => {
  assert.throws(() => new FrameDecoder().push(Buffer.alloc(MAX_FRAME_BYTES + 1, 65)), /FRAME_TOO_LARGE/);
});

test('stdout pollution, primitive frames and truncated input fail closed', () => {
  for (const text of ['log pollution\n', '[]\n', 'null\n', '42\n', '\n']) {
    assert.throws(() => new FrameDecoder().push(Buffer.from(text)));
  }
  const decoder = new FrameDecoder();
  decoder.push(Buffer.from('{'));
  assert.throws(() => decoder.end(), /TRUNCATED_FRAME/);
});

test('malformed UTF-8 is rejected rather than replacing input bytes', () => {
  const bytes = Buffer.concat([Buffer.from('{"text":"'), Buffer.from([0xc3, 0x28]), Buffer.from('"}\n')]);
  assert.throws(() => new FrameDecoder().push(bytes), /encoded data/);
  assert.deepEqual(new FrameDecoder().push(Buffer.from('{"text":"�"}\n')), [{ text: '�' }]);
});
