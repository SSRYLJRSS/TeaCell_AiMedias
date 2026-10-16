import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { GatewayTransport } from '../gateway-transport.mjs';
import { createGatewaySession } from '../gateway-session.mjs';
import { executable, fixture, openai, anthropic } from './helpers/gateway-fixture.mjs';

const route = 'https://teacell-gateway.invalid/v1/chat/completions';
const request = { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer node-must-not-forward' }, body: JSON.stringify({ model: 'mock', messages: [] }) };

for (const protocol of ['openai-completions', 'anthropic-messages']) {
  test(`real Pi ${protocol} codec verifies local HTTPS before forwarding fixture credentials`, { timeout: 15000 }, async t => {
    const respond = (_request, response) => protocol === 'anthropic-messages' ? anthropic(response) : openai(response);
    const f = await fixture(t, respond, { tls: true });
    assert.equal(f.secure, true);
    const session = await createGatewaySession(f.directory, f.bridge, { protocol });
    t.after(() => session.dispose());
    await session.prompt('仅验证隔离 HTTPS');
    const message = session.agent.state.messages.at(-1);
    assert.equal(message.stopReason, 'stop', JSON.stringify(message));
    assert.equal(message.content[0].text, '网关回复');
    assert.equal(f.received.length, 1);
    assert.equal(f.received[0].secure, true);
    assert.equal(f.received[0].headers[protocol === 'anthropic-messages' ? 'x-api-key' : 'authorization'], protocol === 'anthropic-messages' ? 'poc-rust-only-key' : 'Bearer poc-rust-only-key');
    assert.ok(!JSON.stringify(session.agent.state).includes('poc-rust-only-key'));
  });
}

test('Rust gateway rejects an untrusted HTTPS provider before any HTTP request arrives', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response) => openai(response), { tls: true, trustTls: false });
  await assert.rejects(f.bridge.fetch(route, request), /UPSTREAM_FAILED/);
  assert.equal(f.bridge.attempts, 1);
  assert.equal(f.received.length, 0);
  assert.deepEqual(f.bridge.errors.map(error => error.code), ['UPSTREAM_FAILED']);
});

for (const protocol of ['openai-completions', 'anthropic-messages']) {
  test(`real Pi ${protocol} codec traverses Rust IPC with host-only fixture credentials`, { timeout: 15000 }, async t => {
    const f = await fixture(t, (_request, response) => protocol === 'anthropic-messages' ? anthropic(response) : openai(response));
    const session = await createGatewaySession(f.directory, f.bridge, { protocol });
    t.after(() => session.dispose());
    await session.prompt('仅用于隔离测试');
    assert.equal(session.agent.state.messages.at(-1).stopReason, 'stop', JSON.stringify(session.agent.state.messages.at(-1)));
    assert.equal(session.agent.state.messages.at(-1).content[0].text, '网关回复');
    assert.equal(f.bridge.attempts, 1);
    assert.equal(f.received.length, 1);
    assert.equal(f.received[0].headers[protocol === 'anthropic-messages' ? 'x-api-key' : 'authorization'], protocol === 'anthropic-messages' ? 'poc-rust-only-key' : 'Bearer poc-rust-only-key');
    assert.ok(!JSON.stringify(session.agent.state).includes('poc-rust-only-key'));
    assert.deepEqual(session.getActiveToolNames(), []);
  });
}

test('real SDK tool continuation and manual compaction both traverse the same gateway', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response, count) => openai(response, { tool: count === 1 }));
  let calls = 0;
  const session = await createGatewaySession(f.directory, f.bridge, { tools: [{
    name: 'lookup_fixture', label: 'Fixture lookup', description: 'Read only fixture',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    execute: async () => { calls++; return { content: [{ type: 'text', text: '两个演示素材' }], details: {} }; },
  }] });
  t.after(() => session.dispose());
  await session.prompt('查询演示素材');
  assert.equal(calls, 1, JSON.stringify(session.agent.state.messages));
  assert.equal(f.received.length, 2);
  assert.ok(f.received[1].body.messages.some(message => message.role === 'tool'));
  await session.compact('保留任务摘要');
  assert.equal(f.received.length, 3);
  assert.equal(f.bridge.attempts, 3);
});

test('route/model denial cannot send an upstream request', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  await assert.rejects(f.bridge.fetch('https://other.invalid/v1/chat/completions', request), /ROUTE_DENIED/);
  await assert.rejects(f.bridge.fetch(route, { ...request, body: JSON.stringify({ model: 'different' }) }), /MODEL_DENIED/);
  assert.equal(f.received.length, 0);
  assert.equal(f.bridge.attempts, 0);
});

test('response backpressure keeps unread content in Rust and cancellation releases shared permit', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response) => { response.writeHead(200, { 'content-type': 'text/plain' }); response.end('x'.repeat(65536)); });
  const abandoned = await f.bridge.fetch(route, request);
  await delay(60);
  assert.equal(f.bridge.chunks, 0);
  await abandoned.body.cancel();
  const response = await f.bridge.fetch(route, request);
  assert.equal((await response.text()).length, 65536);
  assert.equal(f.received.length, 2);
  assert.equal(f.bridge.attempts, 2);
});

test('redirect cannot carry fixture credentials to a second destination', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response) => { response.writeHead(302, { Location: 'http://127.0.0.1:1/leak' }); response.end(); });
  await assert.rejects(f.bridge.fetch(route, request), /REDIRECT_DENIED/);
  assert.equal(f.received.length, 1);
});

for (const status of [401, 429]) {
  test(`provider ${status} preserves an error and does not auto-switch or auto-retry`, { timeout: 15000 }, async t => {
    const f = await fixture(t, (_request, response) => { response.writeHead(status, { 'content-type': 'application/json' }); response.end(JSON.stringify({ error: { message: 'fixture denied' } })); });
    const session = await createGatewaySession(f.directory, f.bridge);
    t.after(() => session.dispose());
    await session.prompt('测试连接失败');
    assert.equal(session.agent.state.messages.at(-1).stopReason, 'error');
    assert.equal(f.received.length, 1);
    assert.equal(f.bridge.attempts, 1);
  });
}

test('each actual request consumes budget and rejected excess produces no upstream effect', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  for (let index = 0; index < 16; index++) await (await f.bridge.fetch(route, request)).text();
  await assert.rejects(f.bridge.fetch(route, request), /REQUEST_BUDGET/);
  assert.equal(f.received.length, 16);
  assert.equal(f.bridge.attempts, 16);
});

test('automatic threshold compaction crosses the gateway and records a compaction entry', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response, count) => openai(response, { inputTokens: count === 2 ? 3950 : 20 }));
  const session = await createGatewaySession(f.directory, f.bridge, { settings: { compaction: { enabled: true, reserveTokens: 256, keepRecentTokens: 0 } } });
  t.after(() => session.dispose());
  await session.prompt('第一段演示上下文');
  await session.prompt('第二段演示上下文');
  // This fixture cuts inside the second turn: Pi summarizes history and the turn prefix separately.
  assert.equal(f.received.length, 4);
  assert.equal(f.bridge.attempts, 4);
  for (const call of f.received.slice(2)) assert.match(call.body.messages[0].content, /context summarization assistant/);
  assert.ok(session.sessionManager.getEntries().some(entry => entry.type === 'compaction'));
});

test('branch summary also uses the registered governed provider', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  const session = await createGatewaySession(f.directory, f.bridge);
  t.after(() => session.dispose());
  await session.prompt('第一条分支');
  const target = session.getUserMessagesForForking()[0].entryId;
  await session.prompt('接着整理素材');
  const result = await session.navigateTree(target, { summarize: true });
  assert.equal(result.cancelled, false);
  assert.ok(result.summaryEntry);
  assert.equal(f.received.length, 3);
  assert.equal(f.bridge.attempts, 3);
});

test('explicit bounded agent retry counts every real upstream attempt', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response, count) => {
    if (count === 1) { response.writeHead(429, { 'content-type': 'application/json' }); response.end('{"error":{"message":"rate limit"}}'); }
    else openai(response);
  });
  const session = await createGatewaySession(f.directory, f.bridge, { settings: { retry: { enabled: true, maxRetries: 1, baseDelayMs: 1, maxAgentDelayMs: 50 } } });
  t.after(() => session.dispose());
  await session.prompt('可重试请求');
  assert.equal(session.agent.state.messages.at(-1).stopReason, 'stop');
  assert.equal(f.received.length, 2);
  assert.equal(f.bridge.attempts, 2);
});

test('SDK abort interrupts a live stream and later request acquires the same connection', { timeout: 15000 }, async t => {
  let signalStarted;
  const started = new Promise(resolve => { signalStarted = resolve; });
  const f = await fixture(t, (_request, response, count) => {
    if (count === 1) { response.writeHead(200, { 'content-type': 'text/event-stream' }); response.flushHeaders(); signalStarted(); }
    else openai(response);
  });
  const session = await createGatewaySession(f.directory, f.bridge);
  t.after(() => session.dispose());
  const run = session.prompt('中断流');
  await started;
  await session.abort();
  await run;
  assert.equal(session.agent.state.messages.at(-1).stopReason, 'aborted');
  await session.prompt('恢复新请求');
  assert.equal(session.agent.state.messages.at(-1).stopReason, 'stop');
  assert.equal(f.received.length, 2);
});

test('truncated provider response compacts then continues through governed requests', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response, count) => openai(response, { finish: count === 2 ? 'length' : 'stop' }));
  const session = await createGatewaySession(f.directory, f.bridge, { settings: { compaction: { enabled: true, reserveTokens: 256, keepRecentTokens: 0 } } });
  t.after(() => session.dispose());
  await session.prompt('已经完成的上轮');
  await session.prompt('输出被截断的本轮');
  assert.equal(session.agent.state.messages.at(-1).stopReason, 'stop');
  assert.ok(session.sessionManager.getEntries().some(entry => entry.type === 'compaction'));
  assert.equal(f.received.length, 5);
  assert.equal(f.bridge.attempts, 5);
  for (const call of f.received.slice(2, 4)) assert.match(call.body.messages[0].content, /context summarization assistant/);
  assert.doesNotMatch(f.received[4].body.messages[0].content, /context summarization assistant/);
});

test('cancellation while waiting for shared connection does not consume an attempt', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  const first = await f.bridge.fetch(route, request);
  const abort = new globalThis.AbortController();
  const waiting = f.bridge.fetch(route, { ...request, signal: abort.signal });
  const rejection = assert.rejects(waiting, error => error.name === 'AbortError');
  await delay(60);
  assert.equal(f.received.length, 1);
  abort.abort();
  await rejection;
  await first.text();
  await (await f.bridge.fetch(route, request)).text();
  assert.equal(f.received.length, 2);
  assert.equal(f.bridge.attempts, 2);
});

test('broken response stream preserves failure and releases the connection', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response, count) => {
    if (count === 1) {
      response.writeHead(200, { 'content-type': 'text/event-stream', 'content-length': '10000' });
      response.write('data: incomplete');
      void delay(40).then(() => response.destroy());
    } else openai(response);
  });
  await assert.rejects(async () => (await f.bridge.fetch(route, request)).text(), /STREAM_FAILED|UPSTREAM_FAILED/);
  await (await f.bridge.fetch(route, request)).text();
  assert.equal(f.received.length, 2);
});

test('failed native gateway launch rejects readiness and remains closable', { timeout: 5000 }, async () => {
  const bridge = new GatewayTransport(executable + '.missing', 1);
  await assert.rejects(bridge.ready, /ENOENT/);
  const exit = await bridge.close();
  assert.notEqual(exit.code, 0);
});

test('a newly constructed SDK session retains the governed provider wrapper', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  const first = await createGatewaySession(f.directory, f.bridge);
  try { await first.prompt('第一段会话'); } finally { first.dispose(); }
  const second = await createGatewaySession(f.directory, f.bridge);
  try {
    await second.prompt('新的会话');
    assert.equal(second.agent.state.messages.at(-1).stopReason, 'stop');
  } finally { second.dispose(); }
  assert.equal(f.bridge.attempts, 2);
  assert.equal(f.received.length, 2);
});
