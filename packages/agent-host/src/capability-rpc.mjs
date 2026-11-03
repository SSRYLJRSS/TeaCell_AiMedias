import { Buffer } from 'node:buffer';
import { randomUUID } from 'node:crypto';
import { clearTimeout, setTimeout } from 'node:timers';
import { MAX_CAPABILITY_PAYLOAD_BYTES, RESOURCE_TOKEN } from './protocol.mjs';

const PROTOCOL_VERSION = 1;
const EXCHANGE_TIMEOUT_MS = 30_000;
const CAPABILITY_ID = /^[a-z][a-z0-9.]{2,63}$/u;
const RESULT_FIELDS = ['protocol', 'instance', 'type', 'requestId', 'capabilityId', 'ok'];

/** Fixed classes only; mirrors docs/contracts/agent-v1.json AgentErrorCode. */
export const CAPABILITY_ERROR_CODES = Object.freeze(new Set([
  'INVALID_INPUT', 'UNSUPPORTED_PROTOCOL', 'UNSUPPORTED_CAPABILITY', 'UNSUPPORTED_PLATFORM',
  'UNAUTHORIZED', 'GRANT_REVOKED', 'REFERENCE_EXPIRED', 'LIBRARY_CHANGED', 'LIBRARY_OFFLINE',
  'OWNERSHIP_UNVERIFIED', 'APPROVAL_REQUIRED', 'PLAN_EXPIRED', 'PLAN_CHANGED',
  'IDEMPOTENCY_CONFLICT', 'RESOURCE_CONFLICT', 'MAINTENANCE_ACTIVE', 'SERVICE_UNAVAILABLE',
  'BUDGET_EXCEEDED', 'OUTCOME_UNKNOWN', 'CANCELLED', 'INTERNAL',
]));

function capabilityError(code, prefix = 'CAPABILITY') {
  const error = new TypeError(`${prefix}_${code}`);
  Object.defineProperty(error, 'code', { value: code, enumerable: false });
  return error;
}

function exactFields(frame, fields) {
  const keys = Object.keys(frame).sort();
  const expected = [...fields].sort();
  return keys.length === expected.length && keys.every((key, index) => key === expected[index]);
}
function decodePayload(encoded) {
  if (typeof encoded !== 'string' || encoded.length === 0 ||
      encoded.length > Math.ceil(MAX_CAPABILITY_PAYLOAD_BYTES / 3) * 4) {
    throw capabilityError('INVALID_INPUT', 'CAPABILITY_RESULT');
  }
  const bytes = Buffer.from(encoded, 'base64');
  if (bytes.length === 0 || bytes.length > MAX_CAPABILITY_PAYLOAD_BYTES ||
      bytes.toString('base64') !== encoded) {
    throw capabilityError('INVALID_INPUT', 'CAPABILITY_RESULT');
  }
  return JSON.parse(bytes.toString('utf8'));
}

/**
 * Correlates Host capability requests with the Rust domain adapter.
 * sendFrame adds the normal Host envelope and writes the frame to stdout.
 */
export function createCapabilityRpc({ instanceId, sendFrame }) {
  if (typeof instanceId !== 'string' || !RESOURCE_TOKEN.test(instanceId) ||
      typeof sendFrame !== 'function') {
    throw new TypeError('CAPABILITY_TRANSPORT_UNAVAILABLE');
  }
  const pending = new Map();
  let closed = false;

  function exchange(capabilityId, args, { signal } = {}) {
    if (closed) return Promise.reject(capabilityError('CANCELLED'));
    if (typeof capabilityId !== 'string' || !CAPABILITY_ID.test(capabilityId)) {
      return Promise.reject(capabilityError('UNSUPPORTED_CAPABILITY', 'CAPABILITY_UNKNOWN'));
    }
    if (signal?.aborted) return Promise.reject(capabilityError('CANCELLED', 'CAPABILITY_ABORTED'));
    let bytes;
    try {
      bytes = Buffer.from(JSON.stringify(args ?? {}), 'utf8');
    } catch {
      return Promise.reject(capabilityError('INVALID_INPUT'));
    }
    if (bytes.length === 0 || bytes.length > MAX_CAPABILITY_PAYLOAD_BYTES) {
      return Promise.reject(capabilityError('INVALID_INPUT', 'CAPABILITY_PAYLOAD'));
    }
    const requestId = `capability_${randomUUID().replaceAll('-', '')}`;
    return new Promise((resolve, reject) => {
      const finish = (settle, value) => {
        if (!pending.has(requestId)) return;
        pending.delete(requestId);
        clearTimeout(timer);
        signal?.removeEventListener?.('abort', onAbort);
        settle(value);
      };
      const onAbort = () => finish(reject, capabilityError('CANCELLED', 'CAPABILITY_ABORTED'));
      const timer = setTimeout(
        () => finish(reject, capabilityError('SERVICE_UNAVAILABLE', 'CAPABILITY_TIMEOUT')),
        EXCHANGE_TIMEOUT_MS,
      );
      signal?.addEventListener?.('abort', onAbort, { once: true });
      pending.set(requestId, { capabilityId, finish, resolve, reject });
      try {
        sendFrame({
          protocol: PROTOCOL_VERSION,
          instance: instanceId,
          requestId,
          type: 'capability.request',
          capabilityId,
          payload: bytes.toString('base64'),
        });
      } catch {
        finish(reject, capabilityError('SERVICE_UNAVAILABLE', 'CAPABILITY_TRANSPORT'));
      }
    });
  }

  function receive(frame) {
    if (!frame || frame.type !== 'capability.result' || typeof frame.requestId !== 'string') {
      throw new Error('CAPABILITY_INVALID_FRAME');
    }
    if (frame.protocol !== PROTOCOL_VERSION || frame.instance !== instanceId) {
      throw new Error('CAPABILITY_INVALID_FRAME');
    }
    const entry = pending.get(frame.requestId);
    if (!entry) return; // late result for an already-settled exchange
    const allowed = frame.ok
      ? [...RESULT_FIELDS, 'payload']
      : [...RESULT_FIELDS, 'code'];
    if (!exactFields(frame, allowed)) {
      throw new Error('CAPABILITY_INVALID_FRAME');
    }
    if (frame.capabilityId !== entry.capabilityId || typeof frame.ok !== 'boolean') {
      throw new Error('CAPABILITY_INVALID_FRAME');
    }
    if (frame.ok) {
      let result;
      try {
        result = decodePayload(frame.payload);
      } catch {
        entry.finish(entry.reject, capabilityError('INTERNAL', 'CAPABILITY_RESULT'));
        return;
      }
      entry.finish(entry.resolve, result);
      return;
    }
    const code = CAPABILITY_ERROR_CODES.has(frame.code) ? frame.code : 'INTERNAL';
    entry.finish(entry.reject, capabilityError(code));
  }

  function close() {
    closed = true;
    for (const entry of pending.values()) {
      entry.finish(entry.reject, capabilityError('CANCELLED', 'CAPABILITY_CLOSED'));
    }
    pending.clear();
  }

  return { exchange, receive, close };
}
