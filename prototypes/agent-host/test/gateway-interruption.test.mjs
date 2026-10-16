import test from 'node:test';
import assert from 'node:assert/strict';
import { writeFile, rename } from 'node:fs/promises';
import { setTimeout as delay } from 'node:timers/promises';
import { setTimeout, clearTimeout } from 'node:timers';
import { performance } from 'node:perf_hooks';
import { fixture, openai } from './helpers/gateway-fixture.mjs';

const route = 'https://teacell-gateway.invalid/v1/chat/completions';
const request = { method: 'POST', body: JSON.stringify({ model: 'mock', messages: [] }) };
async function bounded(promise) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('CONNECTION_NOT_CLOSED')), 1200); })]); }
  finally { clearTimeout(timer); }
}
for (const phase of ['headers', 'body']) {
  for (const action of ['abort', 'revoke']) {
    test(`${action} during stalled ${phase} closes upstream and releases permit before the HTTP timeout`, { timeout: 15000 }, async t => {
      let started; let closed;
      const began = new Promise(resolve => { started = resolve; });
      const upstreamClosed = new Promise(resolve => { closed = resolve; });
      const f = await fixture(t, (_incoming, response, count) => {
        if (count > 1) { openai(response); return; }
        response.once('close', closed);
        if (phase === 'body') { response.writeHead(200, { 'content-type': 'text/plain' }); response.flushHeaders(); }
        started();
        // Deliberately send no headers/body remainder and never end this response.
      }, { governed: true });
      const controller = new globalThis.AbortController();
      const first = f.bridge.fetch(route, { ...request, signal: controller.signal });
      let denied;
      if (phase === 'headers') denied = assert.rejects(first, action === 'abort' ? /Cancelled/ : /CONNECTION_REVOKED/);
      else {
        const response = await first;
        denied = assert.rejects(response.text(), action === 'abort' ? /Cancelled/ : /CONNECTION_REVOKED/);
      }
      await began;
      // Give the consumer's pull time to reach the Rust network-read future.
      await delay(80);
      assert.equal(f.bridge.attempts, 1);
      assert.equal(f.bridge.chunks, 0);
      const begin = performance.now();
      if (action === 'abort') controller.abort();
      else {
        await writeFile(`${f.policyPath}.next`, JSON.stringify({ revision: 1, enabled: false }));
        await rename(`${f.policyPath}.next`, f.policyPath);
      }
      await denied;
      await bounded(upstreamClosed);
      if (action === 'revoke') {
        assert.equal(f.bridge.errors.at(-1).code, 'CONNECTION_REVOKED');
        await writeFile(`${f.policyPath}.next`, JSON.stringify({ revision: 1, enabled: true }));
        await rename(`${f.policyPath}.next`, f.policyPath);
      }
      await (await f.bridge.fetch(route, request)).text();
      assert.ok(performance.now() - begin < 1200, 'must not wait for the old 3-second HTTP timeout');
      assert.equal(f.received.length, 2);
      assert.equal(f.bridge.attempts, 2);
    });
  }
}

test('upstream reset is one failed attempt and only an explicit follow-up sends again', { timeout: 15000 }, async t => {
  const f = await fixture(t, (incoming, response, count) => {
    if (count === 1) incoming.socket.destroy();
    else openai(response);
  });
  await assert.rejects(f.bridge.fetch(route, request), /UPSTREAM_FAILED/);
  await delay(100);
  assert.equal(f.received.length, 1);
  assert.equal(f.bridge.attempts, 1);
  await (await f.bridge.fetch(route, request)).text();
  assert.equal(f.received.length, 2);
  assert.equal(f.bridge.attempts, 2);
});

test('refused loopback connection is one failed attempt and releases the request slot', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  f.server.closeAllConnections();
  await new Promise(resolve => f.server.close(resolve));

  await assert.rejects(f.bridge.fetch(route, request), /UPSTREAM_FAILED/);
  assert.equal(f.received.length, 0);
  assert.equal(f.bridge.attempts, 1);

  // A later explicit call can acquire the same connection permit; the gateway
  // does not replay either failed attempt on its own.
  await assert.rejects(f.bridge.fetch(route, request), /UPSTREAM_FAILED/);
  assert.equal(f.received.length, 0);
  assert.equal(f.bridge.attempts, 2);
});
