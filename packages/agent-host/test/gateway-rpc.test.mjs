import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { setImmediate } from 'node:timers';
import test from 'node:test';
import { createGatewayRpc, gatewayErrorDetails } from '../src/gateway-rpc.mjs';

const instanceId = 'instance_0123456789abcdef';
const lease = {
  conversationId: 'conversation_0123456789',
  runId: 'run_0123456789abcdef',
  leaseId: 'lease_0123456789abcdef',
};

function responseFrame(outgoing, type, extra = {}) {
  return {
    protocol: 1,
    instance: instanceId,
    type,
    requestId: outgoing.requestId,
    conversationId: outgoing.conversationId,
    runId: outgoing.runId,
    leaseId: outgoing.leaseId,
    gatewayRequestId: outgoing.gatewayRequestId,
    ...extra,
  };
}

function fixtureBridge(responseBytes = Buffer.from('data: fixture\n\n'), endUsage = null) {
  const sent = [];
  let rpc;
  let responseOffset = 0;
  rpc = createGatewayRpc({
    instanceId,
    sendFrame: async outgoing => {
      sent.push(outgoing);
      let reply;
      if (outgoing.type === 'gateway.request.open') {
        reply = responseFrame(outgoing, 'gateway.request.credit', { nextChunk: 0, maxBytes: 5 });
      } else if (outgoing.type === 'gateway.request.chunk') {
        reply = responseFrame(outgoing, 'gateway.request.credit', {
          nextChunk: outgoing.chunkSequence + 1, maxBytes: 5,
        });
      } else if (outgoing.type === 'gateway.request.commit') {
        reply = responseFrame(outgoing, 'gateway.response.head', {
          status: 200, contentType: 'text/event-stream; charset=utf-8',
        });
      } else if (outgoing.type === 'gateway.response.pull') {
        if (responseOffset >= responseBytes.length) {
          reply = responseFrame(outgoing, 'gateway.response.end', { usage: endUsage });
        } else {
          const bytes = responseBytes.subarray(responseOffset, responseOffset + 4);
          responseOffset += bytes.length;
          reply = responseFrame(outgoing, 'gateway.response.chunk', {
            pullSequence: outgoing.pullSequence,
            base64: bytes.toString('base64'),
          });
        }
      } else if (outgoing.type === 'gateway.request.cancel') {
        reply = responseFrame(outgoing, 'gateway.request.cancelled');
      } else {
        assert.fail('unexpected gateway request type: ' + outgoing.type);
      }
      rpc.receive(reply);
    },
  });
  return { rpc, sent };
}

test('gateway RPC uploads one credited chunk at a time and pulls SSE only on reader demand', async () => {
  const expectedBody = Buffer.from('fixture request body');
  const { rpc, sent } = fixtureBridge();
  rpc.setLease(lease);
  const response = await rpc.exchange({
    route: 'openai_chat',
    method: 'POST',
    body: expectedBody,
  });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('content-type'), 'text/event-stream');
  assert.equal(sent.filter(frame => frame.type === 'gateway.request.open').length, 1);
  assert.deepEqual(
    sent.filter(frame => frame.type === 'gateway.request.chunk').map(frame => frame.chunkSequence),
    [0, 1, 2, 3],
  );
  assert.deepEqual(
    Buffer.concat(sent.filter(frame => frame.type === 'gateway.request.chunk')
      .map(frame => Buffer.from(frame.base64, 'base64'))),
    expectedBody,
  );
  assert.equal(sent.some(frame => Object.hasOwn(frame, 'url') ||
    Object.hasOwn(frame, 'headers') || Object.hasOwn(frame, 'apiKey')), false);

  const reader = response.body.getReader();
  const first = await reader.read();
  assert.equal(Buffer.from(first.value).toString('utf8'), 'data');
  assert.equal(sent.filter(frame => frame.type === 'gateway.response.pull').length, 1);
  const remainder = await new globalThis.Response(new globalThis.ReadableStream({
    async pull(controller) {
      const item = await reader.read();
      if (item.done) controller.close();
      else controller.enqueue(item.value);
    },
  })).text();
  assert.equal(Buffer.from(first.value).toString('utf8') + remainder, 'data: fixture\n\n');
  assert.equal(sent.filter(frame => frame.type === 'gateway.response.pull').length, 5);
});

test('gateway RPC cancels the exact attempt when its response reader is cancelled', async () => {
  const { rpc, sent } = fixtureBridge(Buffer.from('fixture response'));
  rpc.setLease(lease);
  const response = await rpc.exchange({
    route: 'anthropic_messages',
    method: 'POST',
    body: Buffer.from('{}'),
  });
  const reader = response.body.getReader();
  const first = await reader.read();
  assert.equal(Buffer.from(first.value).toString('utf8'), 'fixt');
  await reader.cancel();
  await new Promise(resolve => setImmediate(resolve));
  const cancel = sent.find(frame => frame.type === 'gateway.request.cancel');
  assert.ok(cancel);
  assert.equal(cancel.conversationId, lease.conversationId);
  assert.equal(cancel.runId, lease.runId);
  assert.equal(cancel.leaseId, lease.leaseId);
  assert.equal(cancel.gatewayRequestId, sent[0].gatewayRequestId);
});

test('gateway RPC refuses missing leases, unsupported routes, and malformed correlated replies', async () => {
  const sent = [];
  let rpc;
  rpc = createGatewayRpc({
    instanceId,
    sendFrame: async outgoing => {
      sent.push(outgoing);
      if (outgoing.type === 'gateway.request.cancel') {
        rpc.receive(responseFrame(outgoing, 'gateway.request.cancelled'));
      }
    },
  });
  await assert.rejects(rpc.exchange({
    route: 'openai_chat', method: 'POST', body: Buffer.from('{}'),
  }), /GATEWAY_LEASE_UNAVAILABLE/);

  rpc.setLease(lease);
  await assert.rejects(rpc.exchange({
    route: 'arbitrary', method: 'POST', body: Buffer.from('{}'),
  }), /GATEWAY_ROUTE_DENIED/);
  assert.equal(sent.length, 0);

  const pending = rpc.exchange({
    route: 'openai_chat', method: 'POST', body: Buffer.from('{}'),
  });
  await new Promise(resolve => setImmediate(resolve));
  const open = sent[0];
  assert.equal(rpc.receive(responseFrame(open, 'gateway.request.credit', {
    nextChunk: 0, maxBytes: 5, unexpected: true,
  })), true);
  await assert.rejects(pending, /GATEWAY_REQUEST_INVALID/);
});

test('gateway RPC preserves only the fixed failure category and safe HTTP status', async () => {
  let rpc;
  rpc = createGatewayRpc({
    instanceId,
    sendFrame: async outgoing => {
      const failure = outgoing.type === 'gateway.request.open'
        ? responseFrame(outgoing, 'gateway.response.error', { code: 'UPSTREAM_HTTP', status: 429 })
        : responseFrame(outgoing, 'gateway.request.cancelled');
      rpc.receive(failure);
    },
  });
  rpc.setLease(lease);

  await assert.rejects(rpc.exchange({
    route: 'openai_chat', method: 'POST', body: Buffer.from('{}'),
  }), error => {
    assert.equal(error.message, 'GATEWAY_UPSTREAM_HTTP');
    assert.deepEqual(gatewayErrorDetails(error), { code: 'UPSTREAM_HTTP', status: 429 });
    return true;
  });
  assert.deepEqual(rpc.failureForRun(lease.runId), { code: 'UPSTREAM_HTTP', status: 429 });

  const untrusted = new TypeError('private endpoint and response body');
  Object.defineProperty(untrusted, 'gatewayError', {
    value: { code: 'UPSTREAM_HTTP', status: 429, endpoint: 'https://private.invalid' },
  });
  assert.deepEqual(gatewayErrorDetails(untrusted), { code: 'UPSTREAM_HTTP', status: 429 });
});

test('gateway RPC abort releases a pending frame and emits a correlated cancel', async () => {
  const sent = [];
  let rpc;
  rpc = createGatewayRpc({
    instanceId,
    sendFrame: async outgoing => {
      sent.push(outgoing);
      if (outgoing.type === 'gateway.request.cancel') {
        rpc.receive(responseFrame(outgoing, 'gateway.request.cancelled'));
      }
    },
  });
  rpc.setLease(lease);
  const controller = new globalThis.AbortController();
  const pending = rpc.exchange({
    route: 'openai_chat',
    method: 'POST',
    body: Buffer.from('{}'),
    signal: controller.signal,
  });
  await new Promise(resolve => setImmediate(resolve));
  controller.abort();
  await assert.rejects(pending, /GATEWAY_CANCELLED/);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(sent.map(frame => frame.type), [
    'gateway.request.open',
    'gateway.request.cancel',
  ]);
  assert.equal(sent[0].gatewayRequestId, sent[1].gatewayRequestId);
});

test('gateway RPC abort after response head wakes a reader that starts later', async () => {
  const { rpc, sent } = fixtureBridge(Buffer.from('fixture response'));
  rpc.setLease(lease);
  const controller = new globalThis.AbortController();
  const response = await rpc.exchange({
    route: 'openai_chat',
    method: 'POST',
    body: Buffer.from('{}'),
    signal: controller.signal,
  });
  controller.abort();
  await assert.rejects(response.body.getReader().read(), /GATEWAY_CANCELLED/);
  assert.equal(sent.filter(frame => frame.type === 'gateway.request.cancel').length, 1);
});

test('gateway RPC preserves an abort racing with response-head delivery', async () => {
  const sent = [];
  const controller = new globalThis.AbortController();
  let rpc;
  rpc = createGatewayRpc({
    instanceId,
    sendFrame: async outgoing => {
      sent.push(outgoing);
      if (outgoing.type === 'gateway.request.open') {
        rpc.receive(responseFrame(outgoing, 'gateway.request.credit', {
          nextChunk: 0, maxBytes: 5,
        }));
      } else if (outgoing.type === 'gateway.request.chunk') {
        rpc.receive(responseFrame(outgoing, 'gateway.request.credit', {
          nextChunk: outgoing.chunkSequence + 1, maxBytes: 5,
        }));
      } else if (outgoing.type === 'gateway.request.commit') {
        rpc.receive(responseFrame(outgoing, 'gateway.response.head', {
          status: 200, contentType: 'text/event-stream',
        }));
        controller.abort();
      } else if (outgoing.type === 'gateway.request.cancel') {
        rpc.receive(responseFrame(outgoing, 'gateway.request.cancelled'));
      } else {
        assert.fail('unexpected gateway request type: ' + outgoing.type);
      }
    },
  });
  rpc.setLease(lease);
  const response = await rpc.exchange({
    route: 'openai_chat',
    method: 'POST',
    body: Buffer.from('{}'),
    signal: controller.signal,
  });
  await assert.rejects(response.body.getReader().read(), /GATEWAY_CANCELLED/);
  assert.equal(sent.at(-1).type, 'gateway.request.cancel');
});

test('gateway RPC rejects a malformed terminal usage snapshot', async () => {
  const { rpc } = fixtureBridge(Buffer.from('x'), {
    inputTokens: -1, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0,
  });
  rpc.setLease(lease);
  const response = await rpc.exchange({
    route: 'openai_chat', method: 'POST', body: Buffer.from('{}'),
  });
  await assert.rejects(response.text(), /GATEWAY_REQUEST_INVALID/);
});
