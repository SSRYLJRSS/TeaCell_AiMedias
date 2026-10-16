import { Buffer } from 'node:buffer';

const GATEWAY_ORIGIN = 'https://gateway.invalid';
const MAX_REQUEST_BYTES = 2 * 1024 * 1024;
const ALLOWED_RESPONSE_HEADERS = new globalThis.Headers({ 'content-type': 'text/event-stream' });

class GatewayFetchError extends TypeError {}

/**
 * Build the only fetch implementation allowed for a future leased Agent model.
 * The transport callback receives a fixed route and body only: never the SDK URL,
 * credentials, arbitrary headers, or redirect target. Production must supply the
 * private stdio transport; tests supply an in-memory fixture.
 */
export function createGatewayFetch(exchange) {
  if (typeof exchange !== 'function') throw new TypeError('GATEWAY_TRANSPORT_UNAVAILABLE');

  return async function gatewayFetch(input, init = {}) {
    const request = new globalThis.Request(input, init);
    const route = routeFromPlaceholder(request.url);
    if (request.method !== 'POST') throw new TypeError('GATEWAY_METHOD_DENIED');

    const body = await readBoundedBody(request);
    let response;
    try {
      response = await exchange({
        route,
        method: 'POST',
        body,
        signal: request.signal,
      });
    } catch {
      throw new TypeError(request.signal.aborted ? 'GATEWAY_CANCELLED' : 'GATEWAY_TRANSPORT_FAILED');
    }

    if (!(response instanceof globalThis.Response)) throw new TypeError('GATEWAY_INVALID_RESPONSE');
    if (response.status >= 300 && response.status < 400) {
      await response.body?.cancel().catch(() => {});
      throw new TypeError('GATEWAY_REDIRECT_DENIED');
    }
    if (response.status !== 200 || response.headers.get('content-type')?.split(';', 1)[0].trim().toLowerCase() !== 'text/event-stream') {
      await response.body?.cancel().catch(() => {});
      throw new TypeError('GATEWAY_RESPONSE_DENIED');
    }

    return new globalThis.Response(response.body, {
      status: 200,
      headers: ALLOWED_RESPONSE_HEADERS,
    });
  };
}

function routeFromPlaceholder(rawUrl) {
  const url = new globalThis.URL(rawUrl);
  if (url.origin !== GATEWAY_ORIGIN || url.username || url.password || url.hash) {
    throw new TypeError('GATEWAY_TARGET_DENIED');
  }
  if (url.pathname === '/v1/chat/completions' && !url.search) return 'openai_chat';
  // Pi 1.0.4 uses Anthropic's beta SDK method for Messages and appends exactly
  // beta=true. The Rust bridge receives only the route enum and rebuilds /v1/messages.
  if (url.pathname === '/v1/messages' && url.search === '?beta=true') return 'anthropic_messages';
  throw new TypeError('GATEWAY_ROUTE_DENIED');
}

async function readBoundedBody(request) {
  const reader = request.body?.getReader();
  if (!reader) return new Uint8Array();
  const chunks = [];
  let byteLength = 0;
  try {
    while (true) {
      if (request.signal.aborted) throw new GatewayFetchError('GATEWAY_CANCELLED');
      const { done, value } = await reader.read();
      if (done) break;
      byteLength += value.byteLength;
      if (byteLength > MAX_REQUEST_BYTES) throw new GatewayFetchError('GATEWAY_REQUEST_LIMIT');
      chunks.push(value);
    }
  } catch (error) {
    await reader.cancel().catch(() => {});
    if (request.signal.aborted) throw new GatewayFetchError('GATEWAY_CANCELLED');
    if (error instanceof GatewayFetchError) throw error;
    throw new GatewayFetchError('GATEWAY_REQUEST_BODY_FAILED');
  }
  return Buffer.concat(chunks, byteLength);
}

