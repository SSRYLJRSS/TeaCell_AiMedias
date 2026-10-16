import { TextDecoder } from 'node:util';
import { Buffer } from 'node:buffer';

export const PROTOCOL_VERSION = 1;
export const SCHEMA_VERSION = 3;
export const SDK_VERSION = '1.0.4';
export const MAX_FRAME_BYTES = 64 * 1024;
export const MAX_PROMPT_BYTES = 4096;
export const RESOURCE_TOKEN = /^[A-Za-z0-9_-]{16,128}$/;

// Decode incrementally without retaining an unbounded stdout/stdin buffer.
export class FrameDecoder {
  #decoder = new TextDecoder('utf-8', { fatal: true });
  #pending = '';
  #bytes = 0;
  push(chunk) {
    const frames = [];
    let start = 0;
    for (let index = 0; index < chunk.length; index += 1) {
      if (chunk[index] !== 10) continue;
      this.#append(chunk.subarray(start, index));
      const text = this.#pending + this.#decoder.decode();
      this.#pending = '';
      this.#bytes = 0;
      this.#decoder = new TextDecoder('utf-8', { fatal: true });
      if (!text.trim()) throw new Error('EMPTY_FRAME');
      const frame = JSON.parse(text);
      if (!frame || Array.isArray(frame) || typeof frame !== 'object') {
        throw new Error('INVALID_FRAME');
      }
      frames.push(frame);
      start = index + 1;
    }
    this.#append(chunk.subarray(start));
    return frames;
  }
  #append(chunk) {
    this.#bytes += chunk.length;
    if (this.#bytes > MAX_FRAME_BYTES) throw new Error('FRAME_TOO_LARGE');
    this.#pending += this.#decoder.decode(chunk, { stream: true });
  }
  end() {
    if (this.#bytes !== 0) throw new Error('TRUNCATED_FRAME');
  }
}

export function encodeFrame(frame) {
  const text = JSON.stringify(frame);
  if (Buffer.byteLength(text) > MAX_FRAME_BYTES) throw new Error('FRAME_TOO_LARGE');
  return text + '\n';
}

export function requireRequestId(value) {
  if (typeof value !== 'string' || !RESOURCE_TOKEN.test(value)) throw new Error('INVALID_REQUEST_ID');
  return value;
}
