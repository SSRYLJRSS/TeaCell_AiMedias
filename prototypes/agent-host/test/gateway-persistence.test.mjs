import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { spawn } from 'node:child_process';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout, clearTimeout } from 'node:timers';
import { childEnvironment } from '../supervisor.mjs';
import { fixture, openai, anthropic } from './helpers/gateway-fixture.mjs';

const script = fileURLToPath(new URL('../fixtures/persisted-session.mjs', import.meta.url));
async function run(t, f, protocol, mode, file) {
  const child = spawn(process.execPath, [script, String(f.port), f.directory, protocol, mode, ...(file ? [file] : [])], {
    cwd: f.directory, env: childEnvironment(process.env), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
  });
  t.after(() => { if (child.exitCode === null && child.signalCode === null) child.kill(); });
  return new Promise((resolve, reject) => {
    let stdout = ''; let stderr = '';
    const timeout = setTimeout(() => { child.kill(); reject(new Error('PERSISTENCE_FIXTURE_TIMEOUT')); }, 12000);
    child.once('error', error => { clearTimeout(timeout); reject(error); });
    child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
    child.stdout.on('data', chunk => { stdout += chunk; if (stdout.length > 65536) child.kill(); });
    child.stderr.on('data', chunk => { stderr += chunk; if (stderr.length > 65536) child.kill(); });
    child.once('close', code => {
      clearTimeout(timeout);
      if (code !== 0) reject(new Error(`PERSISTENCE_FIXTURE_FAILED: ${stderr}`));
      else { try { resolve(JSON.parse(stdout)); } catch { reject(new Error('PERSISTENCE_FIXTURE_OUTPUT')); } }
    });
  });
}

test('damaged transcripts are preserved and cannot silently resume a shortened history', { timeout: 40000 }, async t => {
  const f = await fixture(t, (_request, response, count) => openai(response, { tool: count === 1 }));
  await f.bridge.close();
  const first = await run(t, f, 'openai-completions', 'create');
  const text = await readFile(first.file, 'utf8');
  const entries = text.trimEnd().split('\n').map(line => JSON.parse(line));
  const count = f.received.length;
  for (const [name, content, error] of [
    ['truncated', text.slice(0, -5), 'SESSION_TRUNCATED'],
    ['middle', text.split('\n').map((line, index) => index === 2 ? '{bad JSON}' : line).join('\n'), 'SESSION_CORRUPT'],
    ['future', [JSON.stringify({ ...entries[0], version: 999 }), ...text.trimEnd().split('\n').slice(1)].join('\n') + '\n', 'SESSION_VERSION_UNSUPPORTED'],
    ['duplicate', text + JSON.stringify(entries[1]) + '\n', 'SESSION_CORRUPT'],
  ]) {
    const file = join(f.directory, 'sessions', `${name}.jsonl`);
    await writeFile(file, content);
    await assert.rejects(run(t, f, 'openai-completions', 'inspect', file), new RegExp(error));
    assert.equal(await readFile(file, 'utf8'), content);
    assert.equal(f.received.length, count);
  }
  // A syntactically complete prefix ending at an assistant tool call is not
  // permission to execute it. Construct the interrupted state without IO effects.
  const callIndex = entries.findIndex(entry => entry.type === 'message' && entry.message.role === 'assistant' && entry.message.content.some(part => part.type === 'toolCall'));
  assert.ok(callIndex > 0);
  const pending = join(f.directory, 'sessions', 'pending-tool.jsonl');
  await writeFile(pending, entries.slice(0, callIndex + 1).map(entry => JSON.stringify(entry)).join('\n') + '\n');
  const inspect = await run(t, f, 'openai-completions', 'inspect', pending);
  assert.equal(inspect.calls, 0);
  assert.equal(inspect.attempts, 0);
  assert.equal(inspect.toolResults, 0);
  assert.equal(f.received.length, count);
});

for (const protocol of ['openai-completions', 'anthropic-messages']) {
  test(`persisted ${protocol} transcript restores in fresh processes without replay or outbound`, { timeout: 40000 }, async t => {
    const f = await fixture(t, (_request, response, count) => protocol === 'openai-completions' ? openai(response, { tool: count === 1 }) : anthropic(response));
    await f.bridge.close();
    const first = await run(t, f, protocol, 'create');
    assert.ok(first.file);
    assert.equal(first.restored.messageCount, 0);
    assert.equal(first.compactions, 1);
    assert.equal(first.calls, protocol === 'openai-completions' ? 1 : 0);
    assert.equal(first.toolResults, first.calls);
    const text = await readFile(first.file, 'utf8');
    assert.ok(!text.includes('poc-rust-only-key'));
    const count = f.received.length;
    const inspect = await run(t, f, protocol, 'inspect', first.file);
    assert.equal(inspect.id, first.id);
    assert.ok(inspect.restored.messageCount > 0);
    assert.equal(inspect.restored.entryCount, first.entryCount);
    assert.equal(inspect.calls, 0);
    assert.equal(inspect.attempts, 0);
    assert.equal(inspect.compactions, 1);
    assert.equal(inspect.toolResults, first.toolResults);
    assert.equal(f.received.length, count);
    const resumed = await run(t, f, protocol, 'resume', first.file);
    assert.equal(resumed.id, first.id);
    assert.equal(resumed.calls, 0);
    assert.equal(resumed.restored.attempts, 0);
    assert.equal(resumed.attempts, 1);
    assert.equal(f.received.length, count + 1);
    assert.deepEqual(resumed.tools, ['lookup_fixture']);
    assert.ok(f.received.at(-1).body.messages.some(message => JSON.stringify(message).includes('用户明确继续后的新一轮')));
    assert.ok(f.received.every(request => request.headers[protocol === 'openai-completions' ? 'authorization' : 'x-api-key'] === (protocol === 'openai-completions' ? 'Bearer poc-rust-only-key' : 'poc-rust-only-key')));
  });
}
