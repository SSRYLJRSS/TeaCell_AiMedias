import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createOfflineSession } from '../src/sdk-session.mjs';

function openAiFixtureResponse(model, finishReason = 'stop') {
  const events = [
    { id: 'chatcmpl-fixture', object: 'chat.completion.chunk', created: 0, model,
      choices: [{ index: 0, delta: { role: 'assistant' }, finish_reason: null }] },
    { id: 'chatcmpl-fixture', object: 'chat.completion.chunk', created: 0, model,
      choices: [{ index: 0, delta: { content: 'fixture session reply' }, finish_reason: null }] },
    { id: 'chatcmpl-fixture', object: 'chat.completion.chunk', created: 0, model,
      choices: [{ index: 0, delta: {}, finish_reason: finishReason }],
      usage: { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 } },
  ];
  const body = events.map(event => 'data: ' + JSON.stringify(event) + '\n\n').join('') +
    'data: [DONE]\n\n';
  return new globalThis.Response(body, {
    status: 200,
    headers: { 'content-type': 'text/event-stream; charset=utf-8' },
  });
}

function anthropicFixtureResponse(model, stopReason = 'end_turn') {
  const events = [
    ['message_start', { type: 'message_start', message: { id: 'msg_fixture',
      type: 'message', role: 'assistant', model, content: [], stop_reason: null,
      stop_sequence: null, usage: { input_tokens: 3, output_tokens: 0 } } }],
    ['content_block_start', { type: 'content_block_start', index: 0,
      content_block: { type: 'text', text: '' } }],
    ['content_block_delta', { type: 'content_block_delta', index: 0,
      delta: { type: 'text_delta', text: 'fixture session reply' } }],
    ['content_block_stop', { type: 'content_block_stop', index: 0 }],
    ['message_delta', { type: 'message_delta',
      delta: { stop_reason: stopReason, stop_sequence: null },
      usage: { output_tokens: 4 } }],
    ['message_stop', { type: 'message_stop' }],
  ];
  const body = events.map(([event, data]) =>
    'event: ' + event + '\ndata: ' + JSON.stringify(data) + '\n\n').join('');
  return new globalThis.Response(body, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
  });
}

for (const fixture of [
  { provider: 'openai', protocol: 'openai_chat', api: 'openai-completions',
    response: openAiFixtureResponse },
  { provider: 'anthropic', protocol: 'anthropic_messages', api: 'anthropic-messages',
    response: anthropicFixtureResponse },
]) {
  test('Pi session selects the leased ' + fixture.protocol + ' model and streams through the gateway fetch', async () => {
    const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-gateway-session-'));
    const conversationId = 'conversation_' + randomUUID().replaceAll('-', '');
    const runId = 'run_' + randomUUID().replaceAll('-', '');
    const leaseId = 'lease_' + randomUUID().replaceAll('-', '');
    const model = 'fixture-model';
    const calls = [];
    let selectedLease;
    let returnLengthResponse = false;
    const gatewayRpc = {
      setLease(value) { selectedLease = value; },
      clearLease() { selectedLease = undefined; },
      async exchange(request) {
        calls.push(request);
        if (!returnLengthResponse) return fixture.response(model);
        const stopReason = fixture.protocol === 'openai_chat' ? 'length' : 'max_tokens';
        return fixture.response(model, stopReason);
      },
    };
    let sessionRecord;
    try {
      sessionRecord = await createOfflineSession(agentDir, conversationId, 'new', {
        gatewayRpc,
        pinSessionFile: async () => true,
        unpinSessionFile: async () => {},
      });

      await assert.rejects(sessionRecord.setGatewayLease({
        leaseId, provider: fixture.provider,
        protocol: fixture.protocol === 'openai_chat' ? 'anthropic_messages' : 'openai_chat',
        model,
      }, { conversationId, runId }), /INVALID_MODEL_LEASE/);
      assert.equal(calls.length, 0);
      assert.equal(selectedLease, undefined);

      await sessionRecord.setGatewayLease({
        leaseId, provider: fixture.provider, protocol: fixture.protocol, model,
      }, { conversationId, runId });
      assert.deepEqual(selectedLease, { conversationId, runId, leaseId });

      const deltas = [];
      const assistantStops = [];
      const compactionStarts = [];
      sessionRecord.session.subscribe(event => {
        if (event.type === 'message_update' && event.assistantMessageEvent.type === 'text_delta') {
          deltas.push(event.assistantMessageEvent.delta);
        }
        if (event.type === 'message_end') assistantStops.push(event.message.stopReason);
        if (event.type === 'compaction_start') compactionStarts.push(event.reason);
      });
      await sessionRecord.session.prompt('hello local fixture');

      assert.equal(deltas.join(''), 'fixture session reply');
      assert.equal(calls.length, 1);
      assert.equal(calls[0].route, fixture.protocol);
      assert.equal(calls[0].method, 'POST');
      const request = JSON.parse(Buffer.from(calls[0].body).toString('utf8'));
      assert.equal(request.model, model);
      assert.equal(request.stream, true);
      assert.equal(request.max_tokens ?? request.max_completion_tokens, 2048);
      assert.equal(JSON.stringify(calls[0]).includes('gateway-placeholder-only'), false);
      assert.equal(JSON.stringify(calls[0]).includes('authorization'), false);

      returnLengthResponse = true;
      await sessionRecord.session.prompt('trigger a synthetic max-output response');
      assert.equal(assistantStops.at(-1), 'length', 'the fixture must reach Pi’s recoverable length-stop path');
      assert.deepEqual(compactionStarts, [], 'disabled automatic compaction must not start on a recoverable length stop');
      assert.equal(calls.length, 2, 'a recoverable length stop must not trigger an implicit compact or retry request');
      const transcript = await readFile(join(agentDir, 'sessions', sessionRecord.pinnedSessionFile), 'utf8');
      assert.equal(transcript.includes('gateway-placeholder-only'), false);
      assert.equal(transcript.includes('"type":"compaction"'), false);

      sessionRecord.clearGatewayLease(runId);
      assert.equal(selectedLease, undefined);
      await assert.rejects(readFile(join(agentDir, 'auth.json')), { code: 'ENOENT' });
      await assert.rejects(readFile(join(agentDir, 'models-cache.json')), { code: 'ENOENT' });
    } finally {
      await sessionRecord?.session.dispose();
      await rm(agentDir, { recursive: true, force: true });
    }
  });
}
