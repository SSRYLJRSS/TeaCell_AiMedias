import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createPrototypeSession } from '../sdk-session.mjs';

test('real Pi SDK streams mock provider with no discovered tools', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-pi-'));
  let session;
  try {
    await writeFile(join(directory, 'AGENTS.md'), 'UNTRUSTED_SENTINEL');
    session = await createPrototypeSession(directory);
    assert.deepEqual(session.getActiveToolNames(), []);
    const events = [];
    session.subscribe(event => events.push(event));
    await session.prompt('仅进行原型测试');
    const deltas = events.filter(event => event.type === 'message_update')
      .map(event => event.assistantMessageEvent)
      .filter(event => event.type === 'text_delta').map(event => event.delta);
    assert.equal(deltas.join(''), '原型流式回复');
    assert.ok(events.some(event => event.type === 'agent_end'));
    assert.ok(!session.agent.state.systemPrompt.includes('UNTRUSTED_SENTINEL'));
  } finally {
    session?.dispose();
    await rm(directory, { recursive: true, force: true });
  }
});

test('stop reasoning aborts a live SDK run', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-pi-'));
  let session;
  try {
    session = await createPrototypeSession(directory);
    const events = [];
    let started;
    const active = new Promise(resolve => { started = resolve; });
    session.subscribe(event => {
      events.push(event);
      if (event.type === 'message_update') started();
    });
    const run = session.prompt('停止测试');
    await active;
    await session.abort();
    await run;
    assert.ok(events.some(event => event.type === 'agent_end'));
    assert.equal(session.agent.state.messages.at(-1).stopReason, 'aborted');
  } finally {
    session?.dispose();
    await rm(directory, { recursive: true, force: true });
  }
});
