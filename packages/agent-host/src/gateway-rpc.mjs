import { Buffer } from 'node:buffer';
import { clearTimeout, setTimeout } from 'node:timers';

const PROTOCOL_VERSION = 1;
const RESOURCE_TOKEN = /^[A-Za-z0-9_-]{16,128}$/u;
const MAX_CHUNK_BYTES = 44 * 1024;
const MAX_REQUEST_BYTES = 2 * 1024 * 1024;
const MAX_RESPONSE_BYTES = 1024 * 1024;
const RPC_TIMEOUT_MS = 180_000;
const CANCEL_TIMEOUT_MS = 2000;
const REPLY_TYPES = new Set([
  'gateway.request.credit', 'gateway.response.head', 'gateway.response.chunk',
  'gateway.response.end', 'gateway.response.error', 'gateway.request.cancelled',
]);
const ERROR_CODES = new Set([
  'ROUTE_DENIED', 'LEASE_REVOKED', 'CONNECTION_CHANGED', 'REQUEST_INVALID',
  'REQUEST_LIMIT', 'BUDGET_EXCEEDED', 'UPSTREAM_UNAVAILABLE', 'UPSTREAM_HTTP',
  'REDIRECT_DENIED', 'STREAM_FAILED', 'CANCELLED', 'TIMEOUT',
]);

function fixedError(code, status) {
  const safeCode = ERROR_CODES.has(code) ? code : 'STREAM_FAILED';
  const error = new TypeError('GATEWAY_' + safeCode);
  const gatewayError = {
    code: safeCode,
    ...(Number.isInteger(status) && status >= 100 && status <= 599 ? { status } : {}),
  };
  Object.defineProperty(error, 'gatewayError', {
    value: Object.freeze(gatewayError),
    enumerable: false,
  });
  return error;
}

export function gatewayErrorDetails(error) {
  let current = error;
  for (let depth = 0; depth < 4 && current && typeof current === 'object'; depth += 1) {
    const details = current.gatewayError;
    if (details && ERROR_CODES.has(details.code) &&
        (details.status === undefined ||
         (Number.isInteger(details.status) && details.status >= 100 && details.status <= 599))) {
      return Object.freeze({
        code: details.code,
        ...(details.status === undefined ? {} : { status: details.status }),
      });
    }
    current = current.cause;
  }
  return undefined;
}

function exactFields(frame, fields) {
  const keys = Object.keys(frame).sort();
  const expected = [...fields].sort();
  return keys.length === expected.length && keys.every((key, index) => key === expected[index]);
}

function validToken(value) {
  return typeof value === 'string' && RESOURCE_TOKEN.test(value);
}

function validateLease(lease) {
  if (!lease || !validToken(lease.conversationId) ||
      !validToken(lease.runId) || !validToken(lease.leaseId)) {
    throw new TypeError('GATEWAY_LEASE_UNAVAILABLE');
  }
}

function decodeChunk(encoded, maxBytes) {
  if (typeof encoded !== 'string' || encoded.length === 0 ||
      encoded.length > Math.ceil(maxBytes / 3) * 4) {
    throw fixedError('REQUEST_LIMIT');
  }
  const bytes = Buffer.from(encoded, 'base64');
  if (bytes.length === 0 || bytes.length > maxBytes || bytes.toString('base64') !== encoded) {
    throw fixedError('REQUEST_INVALID');
  }
  return new Uint8Array(bytes);
}

/**
 * Correlates the Host half of the A3-02 gateway protocol over its private stdio.
 * sendFrame adds the normal Host envelope and writes the frame to stdout.
 */
export function createGatewayRpc({ instanceId, sendFrame }) {
  if (!validToken(instanceId) || typeof sendFrame !== 'function') {
    throw new TypeError('GATEWAY_TRANSPORT_UNAVAILABLE');
  }

  const pending = new Map();
  let requestSequence = 0;
  let attemptSequence = 0;
  let lease;
  let closed = false;
  const failures = new Map();

  function failureForRun(runId) {
    const failure = failures.get(runId);
    return failure ? Object.freeze({ ...failure }) : undefined;
  }

  function recordFailure(runId, error) {
    const details = gatewayErrorDetails(error);
    if (details) failures.set(runId, details);
  }

  function setLease(nextLease) {
    validateLease(nextLease);
    lease = Object.freeze({
      conversationId: nextLease.conversationId,
      runId: nextLease.runId,
      leaseId: nextLease.leaseId,
    });
  }

  function clearLease() {
    if (lease) failures.delete(lease.runId);
    lease = undefined;
  }

  function receive(frame) {
    if (!frame || typeof frame !== 'object' || Array.isArray(frame) ||
        frame.protocol !== PROTOCOL_VERSION || frame.instance !== instanceId ||
        !REPLY_TYPES.has(frame.type) || typeof frame.requestId !== 'string') {
      throw new TypeError('GATEWAY_REPLY_INVALID');
    }
    const waiter = pending.get(frame.requestId);
    if (!waiter) return false;
    if (frame.conversationId !== waiter.identity.conversationId ||
        frame.runId !== waiter.identity.runId ||
        frame.leaseId !== waiter.identity.leaseId ||
        frame.gatewayRequestId !== waiter.identity.gatewayRequestId ||
        !waiter.expected.has(frame.type)) {
      pending.delete(frame.requestId);
      clearTimeout(waiter.timer);
      waiter.reject(fixedError('REQUEST_INVALID'));
      return true;
    }
    try {
      waiter.validate(frame);
      pending.delete(frame.requestId);
      clearTimeout(waiter.timer);
      waiter.resolve(frame);
    } catch {
      pending.delete(frame.requestId);
      clearTimeout(waiter.timer);
      waiter.reject(fixedError('REQUEST_INVALID'));
    }
    return true;
  }

  function request(type, fields, expected, identity, signal, timeoutMs = RPC_TIMEOUT_MS) {
    if (closed) return Promise.reject(fixedError('CANCELLED'));
    if (signal?.aborted) return Promise.reject(fixedError('CANCELLED'));
    if (!Number.isSafeInteger(requestSequence + 1)) {
      closed = true;
      return Promise.reject(fixedError('REQUEST_LIMIT'));
    }
    const requestId = 'gateway_' + (requestSequence + 1).toString(36) + '_' + instanceId.slice(0, 12);
    requestSequence += 1;
    const frame = { type, requestId, ...identity, ...fields };
    const validators = new Map(expected.map(item => [item.type, item.validate]));
    return new Promise((resolve, reject) => {
      const onAbort = () => {
        const waiter = pending.get(requestId);
        if (!waiter) return;
        pending.delete(requestId);
        clearTimeout(waiter.timer);
        signal?.removeEventListener('abort', onAbort);
        reject(fixedError('CANCELLED'));
      };
      const timer = setTimeout(() => {
        pending.delete(requestId);
        signal?.removeEventListener('abort', onAbort);
        reject(fixedError('TIMEOUT'));
      }, timeoutMs);
      pending.set(requestId, {
        identity,
        expected: new Set(validators.keys()),
        validate: value => validators.get(value.type)?.(value),
        resolve: value => {
          signal?.removeEventListener('abort', onAbort);
          resolve(value);
        },
        reject: error => {
          signal?.removeEventListener('abort', onAbort);
          reject(error);
        },
        timer,
      });
      signal?.addEventListener('abort', onAbort, { once: true });
      Promise.resolve().then(() => sendFrame(frame)).catch(() => {
        const waiter = pending.get(requestId);
        if (!waiter) return;
        pending.delete(requestId);
        clearTimeout(waiter.timer);
        waiter.reject(fixedError('STREAM_FAILED'));
      });
    });
  }

  function identityForAttempt() {
    if (!lease) throw new TypeError('GATEWAY_LEASE_UNAVAILABLE');
    if (!Number.isSafeInteger(attemptSequence + 1)) {
      closed = true;
      throw fixedError('REQUEST_LIMIT');
    }
    attemptSequence += 1;
    return Object.freeze({
      conversationId: lease.conversationId,
      runId: lease.runId,
      leaseId: lease.leaseId,
      gatewayRequestId: `gateway_attempt_${attemptSequence.toString(36)}_${instanceId.slice(0, 12)}`,
    });
  }

  async function exchange({ route, method, body, signal }) {
    if (route !== 'openai_chat' && route !== 'anthropic_messages') throw fixedError('ROUTE_DENIED');
    if (method !== 'POST' || !(body instanceof Uint8Array) ||
        body.byteLength === 0 || body.byteLength > MAX_REQUEST_BYTES) {
      throw fixedError(method === 'POST' ? 'REQUEST_LIMIT' : 'ROUTE_DENIED');
    }
    if (signal?.aborted) throw fixedError('CANCELLED');

    const identity = identityForAttempt();
    let terminal = false;
    let cancelSent = false;
    let terminalError;
    let pullSequence = 0;
    let responseBytes = 0;
    let responseController;
    const expected = (type, validate) => ({ type, validate });
    const cleanupAbort = () => signal?.removeEventListener('abort', onAbort);
    const cancel = reason => {
      if (terminal || cancelSent) return;
      cancelSent = true;
      terminal = true;
      if (reason) terminalError = reason;
      cleanupAbort();
      if (reason) responseController?.error(reason);
      void request('gateway.request.cancel', {}, [
        expected('gateway.request.cancelled', frame => {
          if (!exactFields(frame, ['protocol', 'instance', 'type', 'requestId',
            'conversationId', 'runId', 'leaseId', 'gatewayRequestId'])) {
            throw fixedError('REQUEST_INVALID');
          }
        }),
      ], identity, undefined, CANCEL_TIMEOUT_MS).catch(() => {});
    };
    const onAbort = () => cancel(fixedError('CANCELLED'));
    signal?.addEventListener('abort', onAbort, { once: true });

    try {
      const credit = await request('gateway.request.open', { route, method: 'POST' }, [
        expected('gateway.request.credit', frame => {
          if (!exactFields(frame, ['protocol', 'instance', 'type', 'requestId',
            'conversationId', 'runId', 'leaseId', 'gatewayRequestId', 'nextChunk', 'maxBytes']) ||
              frame.nextChunk !== 0 || !Number.isSafeInteger(frame.maxBytes) ||
              frame.maxBytes < 1 || frame.maxBytes > MAX_CHUNK_BYTES) {
            throw fixedError('REQUEST_INVALID');
          }
        }),
        expected('gateway.response.error', validateGatewayError),
      ], identity, signal);
      if (credit.type === 'gateway.response.error') throw fixedError(credit.code, credit.status);

      let offset = 0;
      let sequence = 0;
      let maxBytes = credit.maxBytes;
      while (offset < body.byteLength) {
        if (signal?.aborted) throw fixedError('CANCELLED');
        const chunk = body.subarray(offset, Math.min(offset + maxBytes, body.byteLength));
        const nextCredit = await request('gateway.request.chunk', {
          chunkSequence: sequence,
          base64: Buffer.from(chunk).toString('base64'),
        }, [
          expected('gateway.request.credit', frame => {
            if (!exactFields(frame, ['protocol', 'instance', 'type', 'requestId',
              'conversationId', 'runId', 'leaseId', 'gatewayRequestId', 'nextChunk', 'maxBytes']) ||
                frame.nextChunk !== sequence + 1 || !Number.isSafeInteger(frame.maxBytes) ||
                frame.maxBytes < 1 || frame.maxBytes > MAX_CHUNK_BYTES) {
              throw fixedError('REQUEST_INVALID');
            }
          }),
          expected('gateway.response.error', validateGatewayError),
        ], identity, signal);
        if (nextCredit.type === 'gateway.response.error') {
          throw fixedError(nextCredit.code, nextCredit.status);
        }
        offset += chunk.byteLength;
        sequence += 1;
        maxBytes = nextCredit.maxBytes;
      }

      const head = await request('gateway.request.commit', {
        chunkCount: sequence,
        byteLength: body.byteLength,
      }, [
        expected('gateway.response.head', frame => {
          if (!exactFields(frame, ['protocol', 'instance', 'type', 'requestId',
            'conversationId', 'runId', 'leaseId', 'gatewayRequestId', 'status', 'contentType']) ||
              frame.status !== 200 ||
              typeof frame.contentType !== 'string' ||
              frame.contentType.split(';', 1)[0].trim().toLowerCase() !== 'text/event-stream') {
            throw fixedError(Number.isInteger(frame.status) && frame.status >= 300 && frame.status < 400
              ? 'REDIRECT_DENIED' : 'UPSTREAM_HTTP', frame.status);
          }
        }),
        expected('gateway.response.error', validateGatewayError),
      ], identity, signal);
      if (head.type === 'gateway.response.error') throw fixedError(head.code, head.status);

      const responseBody = new globalThis.ReadableStream({
        start: controller => {
          responseController = controller;
          if (terminalError) controller.error(terminalError);
        },
        pull: async controller => {
          if (terminal) return;
          if (signal?.aborted) {
            cancel(fixedError('CANCELLED'));
            return;
          }
          const sequenceForPull = pullSequence;
          try {
            const frame = await request('gateway.response.pull', {
              pullSequence: sequenceForPull,
            }, [
              expected('gateway.response.chunk', value => {
                if (!exactFields(value, ['protocol', 'instance', 'type', 'requestId',
                  'conversationId', 'runId', 'leaseId', 'gatewayRequestId', 'pullSequence', 'base64']) ||
                    value.pullSequence !== sequenceForPull) {
                  throw fixedError('REQUEST_INVALID');
                }
                value.decoded = decodeChunk(value.base64, MAX_CHUNK_BYTES);
                responseBytes += value.decoded.byteLength;
                if (responseBytes > MAX_RESPONSE_BYTES) throw fixedError('REQUEST_LIMIT');
              }),
              expected('gateway.response.end', validateGatewayEnd),
              expected('gateway.response.error', validateGatewayError),
            ], identity, signal);
            if (frame.type === 'gateway.response.error') {
              throw fixedError(frame.code, frame.status);
            }
            if (frame.type === 'gateway.response.end') {
              terminal = true;
              failures.delete(identity.runId);
              cleanupAbort();
              controller.close();
              return;
            }
            controller.enqueue(frame.decoded);
            pullSequence += 1;
          } catch (error) {
            const failure = error instanceof TypeError && error.message.startsWith('GATEWAY_')
              ? error : fixedError('STREAM_FAILED');
            recordFailure(identity.runId, failure);
            cancel(failure);
          }
        },
        cancel: () => cancel(),
      }, { highWaterMark: 0 });
      return new globalThis.Response(responseBody, {
        status: 200,
        headers: { 'content-type': 'text/event-stream' },
      });
    } catch (error) {
      cancel();
      cleanupAbort();
      const failure = error instanceof TypeError && error.message.startsWith('GATEWAY_')
        ? error : fixedError(signal?.aborted ? 'CANCELLED' : 'STREAM_FAILED');
      recordFailure(identity.runId, failure);
      throw failure;
    }
  }

  function close() {
    closed = true;
    lease = undefined;
    failures.clear();
    for (const waiter of pending.values()) {
      clearTimeout(waiter.timer);
      waiter.reject(fixedError('CANCELLED'));
    }
    pending.clear();
  }

  return Object.freeze({ setLease, clearLease, receive, exchange, failureForRun, close });
}

function validateGatewayError(frame) {
  const base = ['protocol', 'instance', 'type', 'requestId', 'conversationId',
    'runId', 'leaseId', 'gatewayRequestId'];
  if (!exactFields(frame, [...base, 'code']) && !exactFields(frame, [...base, 'code', 'status'])) {
    throw fixedError('REQUEST_INVALID');
  }
  if (!ERROR_CODES.has(frame.code) ||
      (Object.hasOwn(frame, 'status') &&
       (!Number.isInteger(frame.status) || frame.status < 100 || frame.status > 599))) {
    throw fixedError('REQUEST_INVALID');
  }
}

function validateGatewayEnd(frame) {
  if (!exactFields(frame, ['protocol', 'instance', 'type', 'requestId',
    'conversationId', 'runId', 'leaseId', 'gatewayRequestId', 'usage'])) {
    throw fixedError('REQUEST_INVALID');
  }
  if (frame.usage !== null && (!frame.usage || typeof frame.usage !== 'object' ||
      Array.isArray(frame.usage) || !exactFields(frame.usage,
        ['inputTokens', 'outputTokens', 'cacheReadTokens', 'cacheWriteTokens']) ||
      Object.values(frame.usage).some(value => !Number.isSafeInteger(value) || value < 0))) {
    throw fixedError('REQUEST_INVALID');
  }
}
