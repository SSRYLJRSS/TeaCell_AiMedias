import test from 'node:test';
import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';

import {
  FrameDecoder,
  JSON_RPC_ERRORS,
  MAX_FRAME_BYTES,
  MAX_PAYLOAD_BYTES,
  MCP_METHOD_NAMES,
  MCP_METHODS,
  MCP_PROTOCOL_VERSION,
  BRIDGE_ERROR_CODES,
  decodePayload,
  encodeFrame,
  encodePayload,
} from '../src/protocol.mjs';

test('mcp_surface_is_frozen_to_the_five_contract_methods', () => {
  assert.deepEqual([...MCP_METHOD_NAMES].sort(), [
    'initialize',
    'notifications/initialized',
    'ping',
    'tools/call',
    'tools/list',
  ]);
  assert.equal(MCP_METHODS.TOOLS_CALL, 'tools/call');
  assert.equal(MCP_PROTOCOL_VERSION, '2025-11-25');
  // No sampling / roots / prompts / tasks on this surface.
  for (const forbidden of ['sampling', 'roots', 'prompts', 'tasks']) {
    assert.ok(!MCP_METHOD_NAMES.some((name) => name.includes(forbidden)), forbidden);
  }
  assert.deepEqual(JSON_RPC_ERRORS, {
    PARSE_ERROR: -32700,
    INVALID_REQUEST: -32600,
    METHOD_NOT_FOUND: -32601,
    INVALID_PARAMS: -32602,
    INTERNAL_ERROR: -32603,
  });
});

test('exactly_eight_bridge_error_codes', () => {
  assert.deepEqual([...BRIDGE_ERROR_CODES].sort(), [
    'HOST_UNAVAILABLE',
    'INVALID_INPUT',
    'LIBRARY_OFFLINE',
    'NOT_ALLOWED',
    'PAIRING_REQUIRED',
    'PAIRING_REVOKED',
    'PROTOCOL_UNSUPPORTED',
    'UNSUPPORTED',
  ]);
});

test('decoder_handles_frames_split_across_chunks', () => {
  const decoder = new FrameDecoder();
  const raw = Buffer.from('{"type":"ping"}\n{"type":"welcome"}\n', 'utf8');
  const first = decoder.push(raw.subarray(0, 5));
  assert.equal(first.frames.length, 0);
  const second = decoder.push(raw.subarray(5, 20));
  const third = decoder.push(raw.subarray(20));
  const frames = [...first.frames, ...second.frames, ...third.frames];
  assert.equal(frames.length, 2);
  assert.deepEqual(frames[0], { type: 'ping' });
  assert.deepEqual(frames[1], { type: 'welcome' });
});

test('decoder_rejects_empty_invalid_utf8_and_non_object_frames', () => {
  const decoder = new FrameDecoder();
  const result = decoder.push(
    Buffer.concat([
      Buffer.from('\n', 'utf8'),
      Buffer.from('not json\n', 'utf8'),
      Buffer.from([0xff, 0xfe, 0x0a]),
      Buffer.from('[1,2,3]\n', 'utf8'),
      Buffer.from('{"ok":true}\n', 'utf8'),
    ]),
  );
  assert.equal(result.frames.length, 1);
  assert.deepEqual(result.frames[0], { ok: true });
  const codes = result.errors.map((error) => error.code);
  assert.deepEqual(codes, ['EMPTY_FRAME', 'INVALID_JSON', 'INVALID_UTF8', 'NOT_AN_OBJECT']);
});

test('decoder_drops_oversized_frames_and_resynchronises', () => {
  const decoder = new FrameDecoder();
  const huge = Buffer.from(`${'x'.repeat(MAX_FRAME_BYTES + 10)}\n`, 'utf8');
  const result = decoder.push(Buffer.concat([huge, Buffer.from('{"type":"ping"}\n', 'utf8')]));
  assert.deepEqual(result.frames, [{ type: 'ping' }]);
  assert.deepEqual(result.errors.map((error) => error.code), ['FRAME_TOO_LARGE']);
});

test('decoder_accepts_a_frame_exactly_at_the_bound', () => {
  const decoder = new FrameDecoder();
  const prefix = '{"type":"ping","pad":"';
  const suffix = '"}';
  const fill = 'a'.repeat(MAX_FRAME_BYTES - prefix.length - suffix.length);
  const line = `${prefix}${fill}${suffix}`;
  assert.equal(Buffer.byteLength(line, 'utf8'), MAX_FRAME_BYTES);
  const result = decoder.push(Buffer.from(`${line}\n`, 'utf8'));
  assert.equal(result.errors.length, 0);
  assert.equal(result.frames.length, 1);
});

test('decoder_reports_truncated_frame_at_eof', () => {
  const decoder = new FrameDecoder();
  decoder.push(Buffer.from('{"type":"pi', 'utf8'));
  assert.deepEqual(decoder.end().errors.map((error) => error.code), ['TRUNCATED_FRAME']);
  assert.deepEqual(decoder.end().errors, []);
});

test('encode_frame_refuses_to_exceed_the_bound', () => {
  const frame = { type: 'ping', pad: 'a'.repeat(MAX_FRAME_BYTES) };
  assert.throws(() => encodeFrame(frame), /FRAME_TOO_LARGE/u);
  assert.equal(encodeFrame({ type: 'pong' }), '{"type":"pong"}\n');
});

test('payload_round_trips_as_canonical_base64_json', () => {
  const encoded = encodePayload({ query: '猫', limit: 5 });
  assert.equal(Buffer.from(encoded, 'base64').toString('utf8'), '{"query":"猫","limit":5}');
  assert.deepEqual(decodePayload(encoded), { query: '猫', limit: 5 });
});

test('payload_rejects_non_objects_and_bad_base64', () => {
  assert.throws(() => encodePayload([]), /PAYLOAD_INVALID/u);
  assert.throws(() => encodePayload(null), /PAYLOAD_INVALID/u);
  assert.throws(() => decodePayload(''), /PAYLOAD_INVALID/u);
  assert.throws(() => decodePayload('%%%not-base64%%%'), /PAYLOAD_INVALID/u);
  assert.throws(() => decodePayload(Buffer.from('[1]').toString('base64')), /PAYLOAD_INVALID/u);
});

test('payload_respects_the_32_kib_decoded_bound', () => {
  const big = { blob: 'a'.repeat(MAX_PAYLOAD_BYTES) };
  assert.throws(() => encodePayload(big), /PAYLOAD_TOO_LARGE/u);
  const encoded = Buffer.from('a'.repeat(MAX_PAYLOAD_BYTES + 1)).toString('base64');
  assert.throws(() => decodePayload(encoded), /PAYLOAD_TOO_LARGE/u);
});
