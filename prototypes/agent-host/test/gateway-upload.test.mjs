import test from 'node:test';
import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { setTimeout as delay } from 'node:timers/promises';
import { FrameDecoder, encodeFrame } from '../protocol.mjs';
import { createGatewaySession } from '../gateway-session.mjs';
import { fixture, openai, anthropic } from './helpers/gateway-fixture.mjs';

const route = 'https://teacell-gateway.invalid/v1/chat/completions';
for (const protocol of ['openai-completions', 'anthropic-messages']) {
  test(`large Unicode context survives ${protocol} SDK upload exactly once`, { timeout: 15000 }, async t => {
    const f = await fixture(t, (_request, response) => protocol === 'anthropic-messages' ? anthropic(response) : openai(response));
    const session = await createGatewaySession(f.directory, f.bridge, { protocol });
    t.after(() => session.dispose());
    const prompt = '中文照片🌿\n'.repeat(20000);
    await session.prompt(prompt);
    assert.equal(session.agent.state.messages.at(-1).stopReason, 'stop');
    assert.equal(f.received.length, 1);
    assert.equal(f.bridge.attempts, 1);
    const content = f.received[0].body.messages.find(message => message.role === 'user').content;
    assert.equal(typeof content === 'string' ? content : content.map(part => part.text).join(''), prompt);
  });
}

test('oversized request is rejected locally with no upstream attempt', async t => {
  const f = await fixture(t);
  await assert.rejects(f.bridge.fetch(route, { method: 'POST', body: 'x'.repeat(2 * 1024 * 1024 + 1) }), /BODY_LIMIT/);
  assert.equal(f.received.length, 0);
  assert.equal(f.bridge.attempts, 0);
});

test('an unending producer is stopped at the byte limit before any upstream request', async t => {
  const f = await fixture(t);
  let pulls = 0;
  let cancelled = false;
  const body = new globalThis.ReadableStream({
    pull(controller) { pulls++; controller.enqueue(new Uint8Array(16384)); },
    cancel() { cancelled = true; },
  }, { highWaterMark: 0 });
  await assert.rejects(f.bridge.fetch(route, { method: 'POST', body, duplex: 'half' }), /BODY_LIMIT/);
  assert.equal(cancelled, true);
  assert.equal(pulls, 129);
  assert.equal(f.bridge.attempts, 0);
  assert.equal(f.received.length, 0);
});

test('cancel stops a stalled body producer and a subsequent request still works', async t => {
  const f = await fixture(t);
  const controller = new globalThis.AbortController();
  let cancelled = false;
  let started;
  const ready = new Promise(resolve => { started = resolve; });
  const body = new globalThis.ReadableStream({
    pull() { started(); }, cancel() { cancelled = true; },
  }, { highWaterMark: 0 });
  const pending = f.bridge.fetch(route, { method: 'POST', body, duplex: 'half', signal: controller.signal });
  const rejected = assert.rejects(pending, error => error.name === 'AbortError');
  await ready;
  controller.abort();
  await rejected;
  assert.equal(cancelled, true);
  assert.equal(f.bridge.attempts, 0);
  await (await f.bridge.fetch(route, { method: 'POST', body: JSON.stringify({ model: 'mock', messages: [] }) })).text();
  assert.equal(f.received.length, 1);
});

test('body readers consume admission slots and closing the bridge cancels all stalled producers', { timeout: 12000 }, async t => {
  const f = await fixture(t);
  let started = 0;
  let cancelled = 0;
  const pending = Array.from({ length: 8 }, () => {
    const body = new globalThis.ReadableStream({
      pull() { started++; }, cancel() { cancelled++; },
    }, { highWaterMark: 0 });
    return assert.rejects(f.bridge.fetch(route, { method: 'POST', body, duplex: 'half' }), /GATEWAY_CLOSED/);
  });
  while (started < 8) await delay(5);
  await assert.rejects(f.bridge.fetch(route, { method: 'POST', body: '{}' }), /PENDING_LIMIT/);
  assert.equal(f.bridge.attempts, 0);
  await f.bridge.close();
  await Promise.all(pending);
  assert.equal(cancelled, 8);
  assert.equal(f.received.length, 0);
});

test('a stalled producer times out before upstream and releases its admission slot', { timeout: 12000 }, async t => {
  const f = await fixture(t);
  let cancelled = false;
  const body = new globalThis.ReadableStream({ cancel() { cancelled = true; } }, { highWaterMark: 0 });
  await assert.rejects(f.bridge.fetch(route, { method: 'POST', body, duplex: 'half' }), /BODY_READ_TIMEOUT/);
  assert.equal(cancelled, true);
  assert.equal(f.bridge.attempts, 0);
  await (await f.bridge.fetch(route, { method: 'POST', body: JSON.stringify({ model: 'mock', messages: [] }) })).text();
  assert.equal(f.received.length, 1);
});

test('invalid UTF-8 cannot be silently rewritten into a different JSON request', async t => {
  const f = await fixture(t);
  const prefix = Buffer.from('{"model":"mock","messages":[{"role":"user","content":"');
  const suffix = Buffer.from('"}]}');
  await assert.rejects(f.bridge.fetch(route, { method: 'POST', body: Buffer.concat([prefix, Buffer.from([255]), suffix]) }), /INVALID_BODY/);
  assert.equal(f.bridge.attempts, 0);
  assert.equal(f.received.length, 0);
});

test('synchronous empty chunks yield to user cancellation instead of starving timers', { timeout: 12000 }, async t => {
  const f = await fixture(t);
  const abort = new globalThis.AbortController();
  let pulls = 0;
  let cancelled = false;
  const body = new globalThis.ReadableStream({
    pull(controller) { pulls++; controller.enqueue(new Uint8Array(0)); },
    cancel() { cancelled = true; },
  }, { highWaterMark: 0 });
  const rejected = assert.rejects(f.bridge.fetch(route, { method: 'POST', body, duplex: 'half', signal: abort.signal }), error => error.name === 'AbortError');
  await delay(20);
  abort.abort();
  await rejected;
  assert.ok(pulls > 0);
  assert.equal(cancelled, true);
  assert.equal(f.bridge.attempts, 0);
  assert.equal(f.received.length, 0);
});

test('tiny synchronous chunks preserve the exact request across buffer growth', async t => {
  const f = await fixture(t);
  const bytes = Buffer.from(JSON.stringify({ model: 'mock', messages: [{ role: 'user', content: '🌿中文'.repeat(3000) }] }));
  let offset = 0;
  const body = new globalThis.ReadableStream({ pull(controller) {
    if (offset === bytes.length) controller.close();
    else controller.enqueue(bytes.subarray(offset, ++offset));
  } }, { highWaterMark: 0 });
  await (await f.bridge.fetch(route, { method: 'POST', body, duplex: 'half' })).text();
  assert.deepEqual(f.received[0].body, JSON.parse(bytes.toString('utf8')));
  assert.equal(f.bridge.attempts, 1);
});

test('large tool result uses the governed continuation rather than a direct SDK request', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response, count) => openai(response, { tool: count === 1 }));
  const text = '照片说明🌿'.repeat(20000);
  const session = await createGatewaySession(f.directory, f.bridge, { tools: [{
    name: 'lookup_fixture', label: 'Fixture', description: 'Read fixture only',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    execute: async () => ({ content: [{ type: 'text', text }], details: {} }),
  }] });
  t.after(() => session.dispose());
  await session.prompt('查询');
  assert.equal(session.agent.state.messages.at(-1).stopReason, 'stop');
  assert.equal(f.received.length, 2);
  assert.equal(f.bridge.attempts, 2);
  assert.equal(f.received[1].body.messages.find(message => message.role === 'tool').content, text);
});

test('large upload still rejects an unapproved media block before outbound', async t => {
  const f = await fixture(t);
  await assert.rejects(f.bridge.fetch(route, { method: 'POST', body: JSON.stringify({ model: 'mock', messages: [
    { role: 'user', content: [{ type: 'text', text: 'x'.repeat(100000) }, { type: 'image_url', image_url: { url: 'data:image/png;base64,AAAA' } }] },
  ] }) }), /MEDIA_APPROVAL_REQUIRED/);
  assert.equal(f.received.length, 0);
  assert.equal(f.bridge.attempts, 0);
});

test('JSON escape expansion cannot break the single-frame limit', async t => {
  const f = await fixture(t);
  const text = '\u0000'.repeat(12000);
  await (await f.bridge.fetch(route, { method: 'POST', body: JSON.stringify({ model: 'mock', messages: [{ role: 'user', content: text }] }) })).text();
  assert.equal(f.received[0].body.messages[0].content, text);
  assert.equal(f.bridge.attempts, 1);
});

for (const [name, frames, code] of [
  ['sequence', [{ kind: 'body', seq: 1, data: 'eA==' }], 'UPLOAD_SEQUENCE'],
  ['length', [{ kind: 'commit', seq: 0 }], 'UPLOAD_LENGTH'],
  ['UTF8', [{ kind: 'body', seq: 0, data: '/w==' }, { kind: 'commit', seq: 1 }], 'INVALID_BODY'],
  ['cancel', [{ kind: 'cancel' }], 'CANCELLED'],
  ['idle', [], 'UPLOAD_TIMEOUT'],
]) {
  test(`incomplete upload ${name} cannot contact upstream and releases worker`, { timeout: 15000 }, async t => {
    const f = await fixture(t);
    const decoder = new FrameDecoder();
    const events = [];
    f.bridge.child.stdout.on('data', bytes => events.push(...decoder.push(bytes)));
    const send = frame => f.bridge.child.stdin.write(encodeFrame({ id: 99, ...frame }));
    const wait = async predicate => {
      const end = Date.now() + 10000;
      while (!events.some(predicate)) {
        assert.ok(Date.now() < end, JSON.stringify(events));
        await delay(20);
      }
    };
    send({ kind: 'upload', method: 'POST', url: route, revision: 1, length: 1 });
    await wait(frame => frame.kind === 'uploaded' && frame.seq === 0);
    for (const frame of frames) {
      send(frame);
      if (frame.kind === 'body' && frame.seq === 0) await wait(event => event.kind === 'uploaded' && event.seq === 1);
    }
    await wait(frame => frame.kind === 'error' && frame.code === code);
    assert.equal(f.received.length, 0);
    const response = await f.bridge.fetch(route, { method: 'POST', body: JSON.stringify({ model: 'mock', messages: [] }) });
    await response.text();
    assert.equal(f.received.length, 1);
  });
}
