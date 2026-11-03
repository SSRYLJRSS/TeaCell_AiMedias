import { Buffer, isUtf8 } from 'node:buffer';

/** IPC protocol revision frozen by docs/contracts/mcp-bridge-v1.md. */
export const IPC_PROTOCOL_VERSION = 1;
/** Endpoint file schema revision. */
export const ENDPOINT_SCHEMA_VERSION = 1;

/** One JSON object per line, LF separated, 65_536 bytes per frame excluding the LF. */
export const MAX_FRAME_BYTES = 65_536;
/** Decoded capability payload bound. */
export const MAX_PAYLOAD_BYTES = 32 * 1024;
/** Encoded (Base64) capability payload bound. */
export const MAX_PAYLOAD_ENCODED_BYTES = 44 * 1024;

/** stdio MCP surface is frozen to this version; higher client versions are downgraded. */
export const MCP_PROTOCOL_VERSION = '2025-11-25';
export const SERVER_NAME = 'teacell-mcp-bridge';
export const SERVER_VERSION = '1.0.0';

/** The complete method surface. Anything else is not implemented on purpose. */
export const MCP_METHODS = Object.freeze({
  INITIALIZE: 'initialize',
  INITIALIZED: 'notifications/initialized',
  PING: 'ping',
  TOOLS_LIST: 'tools/list',
  TOOLS_CALL: 'tools/call',
});

export const MCP_METHOD_NAMES = Object.freeze(Object.values(MCP_METHODS));

/**
 * The eight end-to-end codes from contract section 6.
 * Every failure leaving the bridge must carry exactly one of these.
 *
 * `INVALID_INPUT` is here because the host returns it directly on a malformed
 * payload (section 5). Leaving it out made the bridge fold it into the default
 * `HOST_UNAVAILABLE` branch, telling the client to restart a perfectly healthy
 * app when the real problem was its own request body.
 */
export const BRIDGE_ERROR_CODES = Object.freeze([
  'HOST_UNAVAILABLE',
  'PROTOCOL_UNSUPPORTED',
  'PAIRING_REQUIRED',
  'PAIRING_REVOKED',
  'NOT_ALLOWED',
  'LIBRARY_OFFLINE',
  'UNSUPPORTED',
  'INVALID_INPUT',
]);

export const BRIDGE_ERROR_CODE_SET = Object.freeze(new Set(BRIDGE_ERROR_CODES));

/** JSON-RPC 2.0 error numbers used by the stdio surface. */
export const JSON_RPC_ERRORS = Object.freeze({
  PARSE_ERROR: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL_ERROR: -32603,
});

export const CONNECTION_ID_PATTERN = /^mcpconn_[0-9a-fA-F]{16}$/u;
export const REQUEST_ID_PATTERN = /^req_[0-9a-f]{16}$/u;
/** Windows named pipes are addressed as `\\.\pipe\<name>`; the name itself is validated. */
export const PIPE_NAME_PATTERN = /^[A-Za-z0-9._-]{1,128}$/u;

/** Raised for framing/payload problems. `code` is a short stable token, not a bridge error code. */
export class FrameError extends Error {
  constructor(code, detail) {
    super(detail ? `${code}: ${detail}` : code);
    this.name = 'FrameError';
    this.code = code;
  }
}

export function isPlainObject(value) {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
}

/**
 * Incremental LF framing over a byte stream.
 *
 * `push()` never throws: it returns the frames it decoded plus the framing errors it
 * observed, because the stdio surface must survive malformed client input and the IPC
 * surface decides on its own whether to disconnect. Oversized frames are dropped and
 * the decoder resynchronises at the next LF instead of buffering without bound.
 */
export class FrameDecoder {
  #chunks = [];
  #bytes = 0;
  #overflowing = false;

  push(chunk) {
    const frames = [];
    const errors = [];
    if (chunk === undefined || chunk === null) return { frames, errors };
    const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    let start = 0;
    for (let index = 0; index < bytes.length; index += 1) {
      if (bytes[index] !== 0x0a) continue;
      const segment = bytes.subarray(start, index);
      start = index + 1;
      if (this.#overflowing) {
        this.#reset();
        errors.push(new FrameError('FRAME_TOO_LARGE', `>${MAX_FRAME_BYTES}`));
        continue;
      }
      this.#chunks.push(segment);
      this.#bytes += segment.length;
      const line = Buffer.concat(this.#chunks, this.#bytes);
      this.#reset();
      const decoded = decodeLine(line);
      if (decoded.error) errors.push(decoded.error);
      else frames.push(decoded.frame);
    }
    if (start < bytes.length) {
      const tail = bytes.subarray(start);
      if (this.#overflowing) {
        // Already past the bound; keep discarding until the next LF.
      } else {
        this.#bytes += tail.length;
        if (this.#bytes > MAX_FRAME_BYTES) {
          this.#chunks = [];
          this.#overflowing = true;
        } else {
          this.#chunks.push(tail);
        }
      }
    }
    return { frames, errors };
  }

  /** EOF with buffered bytes is a truncated frame; a clean EOF returns no errors. */
  end() {
    const errors = [];
    if (this.#overflowing || this.#bytes > 0) {
      errors.push(new FrameError('TRUNCATED_FRAME', 'EOF_BEFORE_LF'));
    }
    this.#reset();
    return { errors };
  }

  #reset() {
    this.#chunks = [];
    this.#bytes = 0;
    this.#overflowing = false;
  }
}

function decodeLine(line) {
  if (line.length > MAX_FRAME_BYTES) {
    return { error: new FrameError('FRAME_TOO_LARGE', `>${MAX_FRAME_BYTES}`) };
  }
  if (line.length === 0) return { error: new FrameError('EMPTY_FRAME') };
  let text;
  try {
    if (!isUtf8(line)) throw new Error('INVALID_UTF8');
    text = line.toString('utf8');
  } catch {
    return { error: new FrameError('INVALID_UTF8') };
  }
  if (!text.trim()) return { error: new FrameError('EMPTY_FRAME') };
  let frame;
  try {
    frame = JSON.parse(text);
  } catch {
    return { error: new FrameError('INVALID_JSON') };
  }
  if (!isPlainObject(frame)) return { error: new FrameError('NOT_AN_OBJECT') };
  return { frame };
}

/** Serialise one frame. Throws when the frame would exceed the frozen bound. */
export function encodeFrame(frame) {
  const text = JSON.stringify(frame);
  if (typeof text !== 'string') throw new FrameError('FRAME_NOT_SERIALISABLE');
  if (Buffer.byteLength(text, 'utf8') > MAX_FRAME_BYTES) {
    throw new FrameError('FRAME_TOO_LARGE', 'ENCODE');
  }
  return `${text}\n`;
}

/** Standard Base64 of a UTF-8 JSON object, bounded on both the decoded and encoded side. */
export function encodePayload(value) {
  if (!isPlainObject(value)) throw new FrameError('PAYLOAD_INVALID', 'NOT_AN_OBJECT');
  const json = JSON.stringify(value);
  if (typeof json !== 'string') throw new FrameError('PAYLOAD_INVALID', 'NOT_SERIALISABLE');
  const bytes = Buffer.from(json, 'utf8');
  if (bytes.length === 0) throw new FrameError('PAYLOAD_INVALID', 'EMPTY');
  if (bytes.length > MAX_PAYLOAD_BYTES) throw new FrameError('PAYLOAD_TOO_LARGE', 'DECODED');
  const encoded = bytes.toString('base64');
  if (encoded.length > MAX_PAYLOAD_ENCODED_BYTES) {
    throw new FrameError('PAYLOAD_TOO_LARGE', 'ENCODED');
  }
  return encoded;
}

/** Inverse of encodePayload; rejects non-canonical Base64 so re-encoding never lies. */
export function decodePayload(encoded) {
  if (typeof encoded !== 'string' || encoded.length === 0) {
    throw new FrameError('PAYLOAD_INVALID', 'EMPTY');
  }
  if (encoded.length > MAX_PAYLOAD_ENCODED_BYTES) {
    throw new FrameError('PAYLOAD_TOO_LARGE', 'ENCODED');
  }
  if (encoded.length % 4 !== 0 || !/^[A-Za-z0-9+/]+={0,2}$/u.test(encoded)) {
    throw new FrameError('PAYLOAD_INVALID', 'NOT_BASE64');
  }
  const bytes = Buffer.from(encoded, 'base64');
  if (bytes.toString('base64') !== encoded) throw new FrameError('PAYLOAD_INVALID', 'NON_CANONICAL');
  if (bytes.length === 0) throw new FrameError('PAYLOAD_INVALID', 'EMPTY');
  if (bytes.length > MAX_PAYLOAD_BYTES) throw new FrameError('PAYLOAD_TOO_LARGE', 'DECODED');
  let text;
  try {
    if (!isUtf8(bytes)) throw new Error('INVALID_UTF8');
    text = bytes.toString('utf8');
  } catch {
    throw new FrameError('PAYLOAD_INVALID', 'INVALID_UTF8');
  }
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    throw new FrameError('PAYLOAD_INVALID', 'INVALID_JSON');
  }
  if (!isPlainObject(value)) throw new FrameError('PAYLOAD_INVALID', 'NOT_AN_OBJECT');
  return value;
}
