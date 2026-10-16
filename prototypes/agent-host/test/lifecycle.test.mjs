import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PrototypeHost, childEnvironment } from '../supervisor.mjs';

test('environment allowlist excludes keys, ambient PATH, proxies and Node injection', () => {
  assert.deepEqual(childEnvironment({ SystemRoot: 'C:/Windows', TEMP: 'tmp', PATH: 'unsafe',
    OPENAI_API_KEY: 'sentinel', NODE_OPTIONS: '--import=evil', HTTPS_PROXY: 'proxy', HOME: 'home' }),
  { SystemRoot: 'C:/Windows', TEMP: 'tmp', PATH: '' });
});

test('child SDK handshake, streaming, orderly exit and explicit restart', { timeout: 30000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-host-'));
  const host = new PrototypeHost(directory);
  let restarted;
  try {
    await host.ready();
    host.send({ type: 'prompt', runId: 'run-1', text: '测试' });
    await host.wait(h => h.frames.some(f => f.type === 'settled' && f.runId === 'run-1'));
    assert.equal(host.frames.filter(f => f.type === 'delta').map(f => f.text).join(''), '原型流式回复');
    await host.close();
    assert.equal(host.exitCode, 0);
    restarted = new PrototypeHost(directory);
    await restarted.ready();
    assert.notEqual(restarted.instance, host.instance);
  } finally {
    await host.close();
    await restarted?.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('same data directory rejects a second host and keeps first host alive', { timeout: 30000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-host-'));
  const first = new PrototypeHost(directory);
  let second;
  try {
    await first.ready();
    second = new PrototypeHost(directory);
    await assert.rejects(second.ready(), /HOST_EXITED/);
    assert.equal(second.exitCode, 2);
    assert.equal(first.closed, false);
  } finally {
    await first.close();
    await second?.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('EOF of parent control pipe tears down owned host', { timeout: 20000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-host-'));
  const host = new PrototypeHost(directory);
  try {
    await host.ready();
    host.child.stdin.end();
    await host.wait(h => h.closed);
    assert.equal(host.exitCode, 0);
  } finally {
    await host.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('missing runtime fails explicitly without restart loop', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-host-'));
  const host = new PrototypeHost(directory, { executable: join(directory, 'missing-runtime') });
  try { await assert.rejects(host.ready(), /ENOENT/); }
  finally { await host.close(); await rm(directory, { recursive: true, force: true }); }
});

test('child stop is distinct from successful completion and permits a new run', { timeout: 20000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-host-'));
  const host = new PrototypeHost(directory);
  try {
    await host.ready();
    host.send({ type: 'prompt', runId: 'stop-1', text: '取消推理' });
    await host.wait(h => h.frames.some(f => f.type === 'delta' && f.runId === 'stop-1'));
    host.send({ type: 'stop', runId: 'stop-1' });
    await host.wait(h => h.frames.some(f => f.type === 'settled' && f.runId === 'stop-1'));
    assert.equal(host.frames.find(f => f.type === 'settled' && f.runId === 'stop-1').aborted, true);
    host.send({ type: 'prompt', runId: 'next-1', text: '新推理' });
    await host.wait(h => h.frames.some(f => f.type === 'settled' && f.runId === 'next-1'));
    assert.equal(host.frames.find(f => f.type === 'settled' && f.runId === 'next-1').aborted, false);
  } finally { await host.close(); await rm(directory, { recursive: true, force: true }); }
});

test('invalid instance command shuts down without processing a prompt', { timeout: 20000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-host-'));
  const host = new PrototypeHost(directory);
  try {
    await host.ready();
    host.child.stdin.write('{"protocol":1,"instance":"wrong","type":"prompt","runId":"x","text":"unsafe"}\n');
    await host.wait(h => h.closed);
    assert.equal(host.exitCode, 3);
    assert.equal(host.frames.filter(f => f.type === 'delta').length, 0);
  } finally { await host.close(); await rm(directory, { recursive: true, force: true }); }
});
