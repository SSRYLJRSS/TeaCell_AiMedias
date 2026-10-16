import test from 'node:test';
import assert from 'node:assert/strict';
import { writeFile, rename, readFile } from 'node:fs/promises';
import { URL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { fixture, openai } from './helpers/gateway-fixture.mjs';
import { createGatewaySession } from '../gateway-session.mjs';

const route = 'https://teacell-gateway.invalid/v1/chat/completions';
const request = { method: 'POST', body: JSON.stringify({ model: 'mock', messages: [] }) };
async function replacePolicy(path, value) {
  await writeFile(`${path}.next`, typeof value === 'string' ? value : JSON.stringify(value));
  await rename(`${path}.next`, path);
}

test('revoking a fixture connection interrupts a live stream and denies queued attempts', { timeout: 15000 }, async t => {
  const f = await fixture(t, (_request, response) => {
    response.writeHead(200, { 'content-type': 'text/plain' }); response.flushHeaders();
  }, { governed: true });
  const first = await f.bridge.fetch(route, request);
  const pending = f.bridge.fetch(route, request);
  const denied = assert.rejects(pending, /CONNECTION_REVOKED/);
  await delay(60);
  assert.equal(f.received.length, 1);
  await replacePolicy(f.policyPath, { revision: 1, enabled: false });
  await assert.rejects(first.text(), /CONNECTION_REVOKED/);
  await denied;
  assert.equal(f.received.length, 1);
  assert.equal(f.bridge.attempts, 1);
  await assert.rejects(f.bridge.fetch(route, request), /CONNECTION_REVOKED/);
  assert.equal(f.bridge.attempts, 1);
});

test('changed revision and malformed trusted policy deny requests before outbound', { timeout: 15000 }, async t => {
  const f = await fixture(t, undefined, { governed: true });
  await replacePolicy(f.policyPath, { revision: 2, enabled: true });
  await assert.rejects(f.bridge.fetch(route, request), /CONNECTION_CHANGED/);
  await replacePolicy(f.policyPath, '{broken');
  await assert.rejects(f.bridge.fetch(route, request), /POLICY_UNAVAILABLE/);
  await replacePolicy(f.policyPath, { revision: 1, enabled: true, approved: true });
  await assert.rejects(f.bridge.fetch(route, request), /POLICY_UNAVAILABLE/);
  assert.equal(f.received.length, 0);
  assert.equal(f.bridge.attempts, 0);
  await replacePolicy(f.policyPath, { revision: 1, enabled: true });
  await (await f.bridge.fetch(route, request)).text();
  assert.equal(f.received.length, 1);
});

test('switching to a newly authorized connection uses its revision and target only', { timeout: 15000 }, async t => {
  const oldConnection = await fixture(t, undefined, { governed: true, revision: 1 });
  await (await oldConnection.bridge.fetch(route, request)).text();
  assert.equal(oldConnection.received.length, 1);

  await replacePolicy(oldConnection.policyPath, { revision: 2, enabled: true });
  oldConnection.bridge.fixtureRevision = 2;
  await assert.rejects(oldConnection.bridge.fetch(route, request), /CONNECTION_CHANGED/);
  assert.equal(oldConnection.received.length, 1);
  assert.equal(oldConnection.bridge.attempts, 1);

  const newConnection = await fixture(t, undefined, { governed: true, revision: 2 });
  await (await newConnection.bridge.fetch(route, request)).text();
  assert.equal(newConnection.received.length, 1);
  assert.equal(newConnection.bridge.attempts, 1);
  assert.equal(oldConnection.received.length, 1);
  assert.equal(oldConnection.bridge.attempts, 1);
});

for (const protocol of ['openai-completions', 'anthropic-messages']) {
  test(`real Pi ${protocol} image request is denied without preview approval`, { timeout: 15000 }, async t => {
    const f = await fixture(t, (_request, response) => openai(response));
    const session = await createGatewaySession(f.directory, f.bridge, { protocol, fixtureImageCapability: true });
    t.after(() => session.dispose());
    const png = await readFile(new URL('../../../src-tauri/icons/Square30x30Logo.png', import.meta.url));
    await session.prompt('识别这张测试图', { images: [{ type: 'image', data: png.toString('base64'), mimeType: 'image/png' }] });
    assert.ok(session.agent.state.messages.some(message => message.role === 'user' && message.content.some(item => item.type === 'image')), JSON.stringify(session.agent.state.messages));
    const result = session.agent.state.messages.at(-1);
    assert.equal(result.stopReason, 'error');
    assert.equal(f.bridge.errors.at(-1).code, 'MEDIA_APPROVAL_REQUIRED');
    assert.equal(f.received.length, 0);
    assert.equal(f.bridge.attempts, 0);
  });
}
