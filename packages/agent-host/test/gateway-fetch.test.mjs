import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { once } from 'node:events';
import test from 'node:test';
import { normalizeContext } from '@earendil-works/pi-ai';
import { streamSimple as anthropicStreamSimple } from '@earendil-works/pi-ai/api/anthropic-messages';
import { streamSimple as openAiStreamSimple } from '@earendil-works/pi-ai/api/openai-completions';
import { createGatewayFetch } from '../src/gateway-fetch.mjs';

const FIXTURE_KEY = 'fixture-only-placeholder';

function fixtureModel(protocol) {
  if (protocol === 'openai_chat') {
    return {
      id: 'fixture-model', name: 'fixture-model', api: 'openai-completions', provider: 'fixture-openai',
      baseUrl: 'https://gateway.invalid/v1', input: ['text'], reasoning: false,
      contextWindow: 8192, maxTokens: 2048,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      compat: {
        supportsStore: false,
        supportsDeveloperRole: false,
        supportsReasoningEffort: false,
        supportsUsageInStreaming: true,
        sendSessionAffinityHeaders: false,
        maxTokensField: 'max_tokens',
      },
    };
  }
  return {
    id: 'fixture-model', name: 'fixture-model', api: 'anthropic-messages', provider: 'fixture-anthropic',
    baseUrl: 'https://gateway.invalid', input: ['text'], reasoning: false,
    contextWindow: 8192, maxTokens: 2048,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  };
}

function fixtureResponse(protocol) {
  const events = protocol === 'openai_chat'
    ? [
      { id: 'chatcmpl-fixture', object: 'chat.completion.chunk', created: 0, model: 'fixture-model', choices: [{ index: 0, delta: { role: 'assistant' }, finish_reason: null }] },
      { id: 'chatcmpl-fixture', object: 'chat.completion.chunk', created: 0, model: 'fixture-model', choices: [{ index: 0, delta: { content: 'fixture reply' }, finish_reason: null }] },
      { id: 'chatcmpl-fixture', object: 'chat.completion.chunk', created: 0, model: 'fixture-model', choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 1, completion_tokens: 2, total_tokens: 3 } },
    ].map(value => `data: ${JSON.stringify(value)}\n\n`).join('') + 'data: [DONE]\n\n'
    : [
      ['message_start', { type: 'message_start', message: { id: 'msg_fixture', type: 'message', role: 'assistant', model: 'fixture-model', content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 1, output_tokens: 0 } } }],
      ['content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }],
      ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'fixture reply' } }],
      ['content_block_stop', { type: 'content_block_stop', index: 0 }],
      ['message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 2 } }],
      ['message_stop', { type: 'message_stop' }],
    ].map(([event, value]) => `event: ${event}\ndata: ${JSON.stringify(value)}\n\n`).join('');
  return new globalThis.Response(events, { status: 200, headers: { 'content-type': 'text/event-stream; charset=utf-8' } });
}

async function runFixture(protocol, options = {}) {
  let captured;
  const fetch = createGatewayFetch(async request => {
    captured = request;
    return fixtureResponse(protocol);
  });
  const model = fixtureModel(protocol);
  const context = normalizeContext({ messages: [{ role: 'user', content: 'hello fixture' }] });
  const stream = (protocol === 'openai_chat' ? openAiStreamSimple : anthropicStreamSimple)(model, context, {
    apiKey: FIXTURE_KEY,
    fetch,
    signal: options.signal,
    cacheRetention: 'none',
    maxTokens: 2048,
    maxRetries: 0,
    env: { PI_CACHE_RETENTION: 'long' },
  });
  const result = await stream.result();
  assert.ok(captured, result.errorMessage ?? 'Pi codec did not reach the supplied gateway fetch');
  return { captured, result };
}

test('Pi 1.0.4 OpenAI Chat codec emits only the allowlisted text request through the placeholder gateway', async () => {
  const { captured, result } = await runFixture('openai_chat');
  assert.equal(captured.route, 'openai_chat');
  assert.equal(captured.method, 'POST');
  assert.deepEqual(Object.keys(captured), ['route', 'method', 'body', 'signal']);
  assert.equal(captured.signal.aborted, false);
  assert.equal(Buffer.from(captured.body).toString('utf8'), JSON.stringify({
    model: 'fixture-model',
    messages: [{ role: 'user', content: 'hello fixture' }],
    stream: true,
    stream_options: { include_usage: true },
    max_tokens: 2048,
  }));
  assert.equal(JSON.stringify(captured).includes(FIXTURE_KEY), false);
  assert.equal(result.content[0]?.text, 'fixture reply');
  assert.equal(result.usage.output, 2);
});

test('Pi 1.0.4 Anthropic codec disables default prompt caching and emits the allowlisted text request', async () => {
  const { captured, result } = await runFixture('anthropic_messages');
  assert.equal(captured.route, 'anthropic_messages');
  assert.equal(captured.method, 'POST');
  assert.deepEqual(Object.keys(captured), ['route', 'method', 'body', 'signal']);
  assert.deepEqual(JSON.parse(Buffer.from(captured.body).toString('utf8')), {
    model: 'fixture-model',
    messages: [{ role: 'user', content: 'hello fixture' }],
    max_tokens: 2048,
    stream: true,
  });
  assert.equal(JSON.stringify(captured).includes(FIXTURE_KEY), false);
  assert.equal(result.content[0]?.text, 'fixture reply');
  assert.equal(result.usage.output, 2);
});

test('gateway fetch rejects arbitrary destinations and upstream redirects', async () => {
  let exchangeCalled = false;
  const fetch = createGatewayFetch(async () => { exchangeCalled = true; return new globalThis.Response('', { status: 302 }); });
  await assert.rejects(fetch('https://example.invalid/v1/chat/completions', {
    method: 'POST', body: '{}', headers: { authorization: 'Bearer not-forwarded' },
  }), /GATEWAY_TARGET_DENIED/);
  await assert.rejects(fetch('https://gateway.invalid/v1/completions', {
    method: 'POST', body: '{}',
  }), /GATEWAY_ROUTE_DENIED/);
  assert.equal(exchangeCalled, false);
  const fetchAllowedRoute = createGatewayFetch(async () => { exchangeCalled = true; return new globalThis.Response('redirect', { status: 302 }); });
  await assert.rejects(fetchAllowedRoute('https://gateway.invalid/v1/chat/completions', {
    method: 'POST', body: '{}',
  }), /GATEWAY_REDIRECT_DENIED/);
  assert.equal(exchangeCalled, true);
});

test('gateway fetch accepts only Pi 1.0.4 Anthropic beta URL shape and passes the route enum', async () => {
  let forwarded;
  const fetch = createGatewayFetch(async request => {
    forwarded = request;
    return fixtureResponse('anthropic_messages');
  });
  await assert.rejects(fetch('https://gateway.invalid/v1/messages', {
    method: 'POST', body: '{}',
  }), /GATEWAY_ROUTE_DENIED/);
  await fetch('https://gateway.invalid/v1/messages?beta=true', {
    method: 'POST', body: '{}',
  });
  assert.equal(forwarded.route, 'anthropic_messages');
  assert.equal(Object.hasOwn(forwarded, 'url'), false);
});

test('gateway fetch passes cancellation to its fixture transport and surfaces a fixed error', async () => {
  const controller = new globalThis.AbortController();
  let observedSignal;
  let markStarted;
  const started = new Promise(resolve => { markStarted = resolve; });
  const fetch = createGatewayFetch(async ({ signal }) => {
    observedSignal = signal;
    markStarted();
    await once(signal, 'abort');
    throw new Error('sensitive transport detail');
  });
  const pending = fetch('https://gateway.invalid/v1/chat/completions', {
    method: 'POST', body: '{}', signal: controller.signal, redirect: 'error',
  });
  await started;
  controller.abort();
  await assert.rejects(pending, /GATEWAY_CANCELLED/);
  assert.equal(observedSignal.aborted, true);
});

test('gateway fetch rejects unsupported methods and request bodies over 2 MiB before transport', async () => {
  let calls = 0;
  const fetch = createGatewayFetch(async () => { calls += 1; return fixtureResponse('openai_chat'); });
  await assert.rejects(fetch('https://gateway.invalid/v1/chat/completions', {
    method: 'GET',
  }), /GATEWAY_METHOD_DENIED/);
  await assert.rejects(fetch('https://gateway.invalid/v1/chat/completions', {
    method: 'POST', body: 'x'.repeat(2 * 1024 * 1024 + 1),
  }), /GATEWAY_REQUEST_LIMIT/);
  assert.equal(calls, 0);
});

test('gateway fetch redacts request body stream failures', async () => {
  let calls = 0;
  const body = new globalThis.ReadableStream({
    start(controller) {
      controller.error(new Error('sensitive request body detail'));
    },
  });
  const fetch = createGatewayFetch(async () => { calls += 1; return fixtureResponse('openai_chat'); });

  await assert.rejects(fetch('https://gateway.invalid/v1/chat/completions', {
    method: 'POST',
    body,
    duplex: 'half',
  }), error => error.message === 'GATEWAY_REQUEST_BODY_FAILED');
  assert.equal(calls, 0);
});
