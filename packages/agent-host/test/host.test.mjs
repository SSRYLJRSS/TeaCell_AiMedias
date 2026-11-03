import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { clearTimeout, setTimeout } from 'node:timers';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { URL, fileURLToPath } from 'node:url';
import { FrameDecoder, encodeFrame, SCHEMA_VERSION } from '../src/protocol.mjs';
import { ensureSessionsDirectory, verifyAgentDirectory } from '../src/paths.mjs';
import { PI_TOOL_NAMES } from '../src/tools.mjs';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const nodePath = join(repositoryRoot, 'artifacts', 'agent-runtime', 'node-v24.19.0-win-x64', 'node.exe');
const bootstrapPath = fileURLToPath(new URL('../src/bootstrap.mjs', import.meta.url));
const appBuild = '1.0.2';
const supportedRuntime = process.platform === 'win32' && process.arch === 'x64';

/**
 * A3-10 的 `conversation.open` 载荷：档位 + 该档应注册的工具集合。
 * 除权限档位用例之外，其它用例都用 `full` 保持既有行为不变。
 */
function openArgs(conversationId, mode, permission = 'full', tools = [...PI_TOOL_NAMES]) {
  return { conversationId, mode, permission, tools };
}

class HostClient {
  constructor(child, instance) {
    this.child = child;
    this.instance = instance;
    this.frames = [];
    this.pinRequests = [];
    this.waiters = [];
    this.stderr = '';
    this.gatewayFailure = undefined;
    this.gatewayRequestOpens = 0;
    this.decoder = new FrameDecoder();
    child.stdout.on('data', chunk => {
      for (const frame of this.decoder.push(chunk)) {
        this.frames.push(frame);
        if (frame.type === 'gateway.request.open') this.gatewayRequestOpens += 1;
        if (frame.type === 'session.pin_request') {
          this.pinRequests.push(frame);
          child.stdin.write(encodeFrame({
            protocol: 1,
            instance: this.instance,
            type: 'session.pin_result',
            pinId: frame.pinId,
            approved: true,
          }));
        }
        if (frame.type === 'gateway.request.open' && this.gatewayFailure) {
          child.stdin.write(encodeFrame({
            protocol: 1,
            instance: this.instance,
            type: 'gateway.response.error',
            requestId: frame.requestId,
            conversationId: frame.conversationId,
            runId: frame.runId,
            leaseId: frame.leaseId,
            gatewayRequestId: frame.gatewayRequestId,
            ...this.gatewayFailure,
          }));
        } else if (frame.type === 'gateway.request.cancel') {
          child.stdin.write(encodeFrame({
            protocol: 1,
            instance: this.instance,
            type: 'gateway.request.cancelled',
            requestId: frame.requestId,
            conversationId: frame.conversationId,
            runId: frame.runId,
            leaseId: frame.leaseId,
            gatewayRequestId: frame.gatewayRequestId,
          }));
        }
        for (const wake of this.waiters.splice(0)) wake();
      }
    });
    child.stdout.on('end', () => this.decoder.end());
    child.stderr.setEncoding('utf8').on('data', chunk => {
      this.stderr = `${this.stderr}${chunk}`.slice(-4096);
    });
  }

  async waitFor(predicate, timeoutMs = 10000) {
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const index = this.frames.findIndex(predicate);
      if (index >= 0) return this.frames.splice(index, 1)[0];
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new Error('HOST_RESPONSE_TIMEOUT');
      await new Promise((resolveWait, reject) => {
        const waiter = () => {
          clearTimeout(timer);
          resolveWait();
        };
        const timer = setTimeout(() => {
          this.waiters = this.waiters.filter(candidate => candidate !== waiter);
          reject(new Error(`HOST_RESPONSE_TIMEOUT; exit=${this.child.exitCode}; frames=${JSON.stringify(this.frames)}; stderr=${this.stderr}`));
        }, remaining);
        this.waiters.push(waiter);
      });
    }
  }

  send(type, payload = {}) {
    const requestId = `request_${randomUUID().replaceAll('-', '')}`;
    this.child.stdin.write(encodeFrame({
      protocol: 1,
      instance: this.instance,
      requestId,
      type,
      ...payload,
    }));
    return requestId;
  }

  async request(type, payload = {}) {
    const requestId = this.send(type, payload);
    return this.waitFor(frame => frame.requestId === requestId && ['ack', 'ready', 'error'].includes(frame.type));
  }
}

function minimalEnvironment() {
  const env = { PATH: '' };
  for (const key of ['SystemRoot', 'WINDIR', 'TEMP', 'TMP', 'LANG', 'LC_ALL']) {
    if (process.env[key]) env[key] = process.env[key];
  }
  return env;
}

function spawnHost(agentDir, mode = 'fresh') {
  const instance = randomUUID();
  const child = spawn(nodePath, [bootstrapPath, agentDir, instance, appBuild, mode], {
    cwd: agentDir,
    env: minimalEnvironment(),
    windowsHide: true,
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const client = new HostClient(child, instance);
  child.stdin.write('start\n');
  client.send('host.hello', { appBuild, schemaVersion: SCHEMA_VERSION, sdkVersion: '1.0.4' });
  return client;
}

test('Host data and session directories reject junctions before writing through them', {
  skip: !supportedRuntime,
}, async t => {
  const parent = await mkdtemp(join(tmpdir(), 'teacell-agent-paths-'));
  const rootTarget = join(parent, 'root-target');
  const rootLink = join(parent, 'root-link');
  const agentDir = join(parent, 'agent-runtime');
  const sessionsTarget = join(parent, 'sessions-target');
  try {
    await Promise.all([mkdir(rootTarget), mkdir(agentDir), mkdir(sessionsTarget)]);
    try {
      await symlink(rootTarget, rootLink, 'junction');
      await symlink(sessionsTarget, join(agentDir, 'sessions'), 'junction');
    } catch (error) {
      t.skip(`Windows junction creation is unavailable: ${error.code}`);
      return;
    }

    await assert.rejects(verifyAgentDirectory(rootLink), /AGENT_DIRECTORY_INVALID/);
    await assert.rejects(ensureSessionsDirectory(agentDir), /SESSION_DIRECTORY_INVALID/);
    assert.deepEqual(await readdir(rootTarget), []);
    assert.deepEqual(await readdir(sessionsTarget), []);
  } finally {
    await rm(parent, { recursive: true, force: true });
  }
});

test('session discovery rejects a junction entry before reading its target', {
  skip: !supportedRuntime,
}, async t => {
  const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-session-link-'));
  const sessionsDir = join(agentDir, 'sessions');
  const outside = join(agentDir, 'outside');
  const conversationId = `conversation_${randomUUID().replaceAll('-', '')}`;
  const outsideSession = `${JSON.stringify({
    type: 'session', version: 3, id: conversationId, timestamp: new Date().toISOString(), cwd: agentDir,
  })}\n`;
  let client;
  try {
    await Promise.all([mkdir(sessionsDir), mkdir(outside)]);
    await writeFile(join(outside, 'session.jsonl'), outsideSession);
    try {
      await symlink(outside, join(sessionsDir, 'linked.jsonl'), 'junction');
    } catch (error) {
      t.skip(`Windows junction creation is unavailable: ${error.code}`);
      return;
    }

    client = spawnHost(agentDir);
    await client.waitFor(frame => frame.type === 'ready');
    const opened = await client.request('conversation.open', openArgs(conversationId, 'resume'));
    assert.equal(opened.type, 'error');
    assert.equal(opened.code, 'SESSION_OPEN_FAILED');
    assert.equal(await readFile(join(outside, 'session.jsonl'), 'utf8'), outsideSession);
    assert.equal((await client.request('host.shutdown')).type, 'ack');
    assert.equal(await waitForExit(client.child), 0);
  } finally {
    await stopClient(client);
    await rm(agentDir, { recursive: true, force: true });
  }
});

async function stopClient(client) {
  if (!client || client.child.exitCode !== null) return;
  const exited = once(client.child, 'exit');
  client.child.kill();
  let timer;
  await Promise.race([exited, new Promise(resolveWait => { timer = setTimeout(resolveWait, 5000); })]);
  clearTimeout(timer);
}

async function waitForExit(child) {
  if (child.exitCode !== null) return child.exitCode;
  let timer;
  try {
    const [code] = await Promise.race([
      once(child, 'exit'),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('HOST_SHUTDOWN_TIMEOUT')), 10000); }),
    ]);
    return code;
  } finally {
    clearTimeout(timer);
  }
}

test('resuming a conversation with no session file reports SESSION_NOT_FOUND, not SESSION_OPEN_FAILED', {
  skip: !supportedRuntime,
  timeout: 30000,
}, async () => {
  const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-missing-session-'));
  let client;
  try {
    client = spawnHost(agentDir);
    await client.waitFor(frame => frame.type === 'ready');
    const conversationId = `conversation_${randomUUID().replaceAll('-', '')}`;
    const resumed = await client.request('conversation.open', openArgs(conversationId, 'resume'));
    assert.equal(resumed.type, 'error');
    // 前端只有在拿到这个码时才允许把 resume 降级成 new；其余错误都必须保留会话。
    assert.equal(resumed.code, 'SESSION_NOT_FOUND');
    assert.equal(client.child.exitCode, null, 'a command rejection must not terminate the Host');

    const recreated = await client.request('conversation.open', openArgs(conversationId, 'new'));
    assert.equal(recreated.type, 'ack');
    assert.equal((await client.request('host.shutdown')).type, 'ack');
    assert.equal(await waitForExit(client.child), 0);
  } finally {
    await stopClient(client);
    await rm(agentDir, { recursive: true, force: true });
  }
});

test('bundled Windows Node runs an offline Pi session, resumes it, and exits cleanly', {
  skip: !supportedRuntime,
  timeout: 30000,
}, async () => {
  const version = spawnSync(nodePath, ['--version'], { encoding: 'utf8', env: minimalEnvironment() });
  assert.equal(version.status, 0);
  assert.equal(version.stdout.trim(), 'v24.19.0');

  const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-host-'));
  const conversationId = `conversation_${randomUUID().replaceAll('-', '')}`;
  let client;
  try {
    client = spawnHost(agentDir);
    const ready = await client.waitFor(frame => frame.type === 'ready');
    assert.equal(ready.protocol, 1);
    assert.equal(ready.schemaVersion, SCHEMA_VERSION);
    assert.equal(ready.sdk, '1.0.4');
    assert.equal(ready.modelNetwork, false);
    assert.equal(ready.gatewayProtocol, 1);
    assert.deepEqual(ready.tools, [...PI_TOOL_NAMES]);

    const opened = await client.request('conversation.open', openArgs(conversationId, 'new'));
    assert.equal(opened.type, 'ack');
    const sessionId = opened.sessionId;
    const runId = `run_${randomUUID().replaceAll('-', '')}`;
    const sendId = client.send('conversation.send', { conversationId, runId, text: '测试离线 Host' });
    assert.equal((await client.waitFor(frame => frame.requestId === sendId && frame.type === 'ack')).runId, runId);
    const delta = await client.waitFor(frame => frame.type === 'delta' && frame.runId === runId);
    assert.equal(delta.text, '离线');
    const settled = await client.waitFor(frame => frame.type === 'settled' && frame.runId === runId);
    assert.equal(settled.aborted, false);

    // conversation.history 只读回读：已打开会话直接投影，必须包含本轮用户文本；
    // 不写库、不重放，且 ACK 精确回本会话 id。
    const history = await client.request('conversation.history', { conversationId });
    assert.equal(history.type, 'ack');
    assert.equal(history.conversationId, conversationId);
    assert.ok(Array.isArray(history.messages));
    assert.ok(history.messages.some(message => message.role === 'user' && message.text === '测试离线 Host'));
    assert.ok(history.messages.every(message => typeof message.id === 'string' && message.id.length > 0));

    const closed = await client.request('conversation.close', { conversationId });
    assert.equal(closed.type, 'ack');
    const resumed = await client.request('conversation.open', openArgs(conversationId, 'resume'));
    assert.equal(resumed.type, 'ack');
    assert.equal(resumed.sessionId, sessionId);

    const authFile = join(agentDir, 'auth.json');
    const modelsFile = join(agentDir, 'models-cache.json');
    await assert.rejects(readFile(authFile), { code: 'ENOENT' });
    await assert.rejects(readFile(modelsFile), { code: 'ENOENT' });
    const shutdown = await client.request('host.shutdown');
    assert.equal(shutdown.type, 'ack');
    const exitCode = await waitForExit(client.child);
    assert.equal(exitCode, 0);
    assert.equal((await readdir(agentDir)).includes('host.lock'), false);
  } finally {
    await stopClient(client);
    await rm(agentDir, { recursive: true, force: true });
  }
});

test('schema v3 validates a model lease before accepting the run', {
  skip: !supportedRuntime,
  timeout: 30000,
}, async () => {
  const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-v2-gateway-'));
  const conversationId = 'conversation_' + randomUUID().replaceAll('-', '');
  let client;
  try {
    client = spawnHost(agentDir);
    const ready = await client.waitFor(frame => frame.type === 'ready');
    assert.equal(ready.schemaVersion, SCHEMA_VERSION);
    assert.equal(ready.gatewayProtocol, 1);
    await client.request('conversation.open', openArgs(conversationId, 'new'));

    const rejectedLease = await client.request('conversation.send', {
      conversationId,
      runId: 'run_' + randomUUID().replaceAll('-', ''),
      text: 'must reject a mismatched leased codec',
      gatewayLease: {
        leaseId: 'lease_' + randomUUID().replaceAll('-', ''),
        provider: 'openai',
        protocol: 'anthropic_messages',
        model: 'fixture-model',
      },
    });
    assert.equal(rejectedLease.type, 'error');
    assert.equal(rejectedLease.code, 'INVALID_MODEL_LEASE');

    const runId = 'run_' + randomUUID().replaceAll('-', '');
    const requestId = client.send('conversation.send', {
      conversationId, runId, text: '离线路径保持可用',
    });
    assert.equal((await client.waitFor(frame => frame.requestId === requestId)).type, 'ack');
    const settled = await client.waitFor(frame => frame.type === 'settled' && frame.runId === runId);
    assert.equal(settled.aborted, false);
    assert.equal(client.frames.some(frame =>
      typeof frame.type === 'string' && frame.type.startsWith('gateway.')), false);
    assert.equal((await client.request('host.shutdown')).type, 'ack');
    assert.equal(await waitForExit(client.child), 0);
  } finally {
    await stopClient(client);
    await rm(agentDir, { recursive: true, force: true });
  }
});

test('schema v3 forwards a safe gateway failure category and status with the run error', {
  skip: !supportedRuntime,
  timeout: 30000,
}, async () => {
  const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-host-gateway-error-'));
  const conversationId = 'conversation_' + randomUUID().replaceAll('-', '');
  const runId = 'run_' + randomUUID().replaceAll('-', '');
  let client;
  try {
    client = spawnHost(agentDir);
    client.gatewayFailure = { code: 'UPSTREAM_HTTP', status: 429 };
    const ready = await client.waitFor(frame => frame.type === 'ready');
    assert.equal(ready.schemaVersion, SCHEMA_VERSION);
    await client.request('conversation.open', openArgs(conversationId, 'new'));
    const requestId = client.send('conversation.send', {
      conversationId,
      runId,
      text: 'trigger a local gateway fixture error',
      gatewayLease: {
        leaseId: 'lease_' + randomUUID().replaceAll('-', ''),
        provider: 'openai',
        protocol: 'openai_chat',
        model: 'fixture-model',
      },
    });
    assert.equal((await client.waitFor(frame => frame.requestId === requestId && frame.type === 'ack')).type, 'ack');
    const failure = await client.waitFor(frame => frame.type === 'error' && frame.runId === runId);
    assert.equal(failure.code, 'RUN_FAILED');
    assert.deepEqual(failure.gatewayError, { code: 'UPSTREAM_HTTP', status: 429 });
    const settled = await client.waitFor(frame => frame.type === 'settled' && frame.runId === runId);
    assert.equal(settled.aborted, true);
    assert.equal(client.gatewayRequestOpens, 1, 'a 429 must not trigger an implicit SDK retry');
    assert.equal((await client.request('host.shutdown')).type, 'ack');
    assert.equal(await waitForExit(client.child), 0);
  } finally {
    await stopClient(client);
    await rm(agentDir, { recursive: true, force: true });
  }
});

test('fresh startup refuses a stale lease; explicit recovery reclaims it', {
  skip: !supportedRuntime,
  timeout: 30000,
}, async () => {
  const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-recovery-'));
  let client;
  try {
    const deadProcess = spawn(process.execPath, ['-e', 'setTimeout(() => process.exit(0), 10)']);
    const deadPid = deadProcess.pid;
    assert.ok(deadPid);
    await once(deadProcess, 'exit');
    const instance = randomUUID();
    const lock = join(agentDir, 'host.lock');
    await mkdir(lock);
    await writeFile(join(lock, 'owner.json'), JSON.stringify({ schema: 1, pid: deadPid, instance }));

    client = spawnHost(agentDir, 'fresh');
    await waitForExit(client.child);
    assert.equal(JSON.parse(await readFile(join(lock, 'owner.json'), 'utf8')).instance, instance);

    client = spawnHost(agentDir, 'recover-stranded');
    const ready = await client.waitFor(frame => frame.type === 'ready');
    assert.equal(ready.type, 'ready');
    const shutdown = await client.request('host.shutdown');
    assert.equal(shutdown.type, 'ack');
    const exitCode = await waitForExit(client.child);
    assert.equal(exitCode, 0);
  } finally {
    await stopClient(client);
    await rm(agentDir, { recursive: true, force: true });
  }
});

test('recoverable command rejection keeps the Host and other sessions alive', {
  skip: !supportedRuntime,
  timeout: 30000,
}, async () => {
  const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-rejection-'));
  let client;
  try {
    client = spawnHost(agentDir);
    await client.waitFor(frame => frame.type === 'ready');
    const firstConversation = `conversation_${randomUUID().replaceAll('-', '')}`;
    const secondConversation = `conversation_${randomUUID().replaceAll('-', '')}`;
    const first = await client.request('conversation.open', openArgs(firstConversation, 'new'));
    assert.equal(first.type, 'ack');

    const duplicate = await client.request('conversation.open', openArgs(firstConversation, 'new'));
    assert.equal(duplicate.type, 'error');
    assert.equal(duplicate.code, 'CONVERSATION_EXISTS');
    assert.equal(client.child.exitCode, null, 'a command rejection must not terminate the Host');

    const second = await client.request('conversation.open', openArgs(secondConversation, 'new'));
    assert.equal(second.type, 'ack');
    const resumed = await client.request('conversation.open', openArgs(firstConversation, 'resume'));
    assert.equal(resumed.type, 'ack');
    assert.equal(resumed.sessionId, first.sessionId);
    assert.ok(client.pinRequests.every(frame => frame.fileName.endsWith(`_${frame.conversationId}.jsonl`)));

    assert.equal((await client.request('host.shutdown')).type, 'ack');
    assert.equal(await waitForExit(client.child), 0);
  } finally {
    await stopClient(client);
    await rm(agentDir, { recursive: true, force: true });
  }
});

test('A3-10 the Host registers exactly the session tool subset Rust sends', {
  skip: !supportedRuntime,
  timeout: 30000,
}, async () => {
  const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-permission-'));
  let client;
  try {
    client = spawnHost(agentDir);
    await client.waitFor(frame => frame.type === 'ready');

    // 档位 → 工具集合的映射只存在于 Rust；Host 侧只证明「照单注册、不做加法」。
    const readTools = [
      'tea_assets_search',
      'tea_operations_cancel',
      'tea_operations_get',
      'tea_workflows_load',
    ];
    const readConversation = `conversation_${randomUUID().replaceAll('-', '')}`;
    const read = await client.request('conversation.open', openArgs(readConversation, 'new', 'read', readTools));
    assert.equal(read.type, 'ack');
    assert.deepEqual(read.tools, readTools);

    // 重开会话不重建工具表，回读的仍必须是原来的裁剪结果。
    const resumedRead = await client.request('conversation.open', openArgs(readConversation, 'resume', 'read', readTools));
    assert.equal(resumedRead.type, 'ack');
    assert.equal(resumedRead.sessionId, read.sessionId);
    assert.deepEqual(resumedRead.tools, readTools);

    const fullConversation = `conversation_${randomUUID().replaceAll('-', '')}`;
    const full = await client.request('conversation.open', openArgs(fullConversation, 'new', 'full', [...PI_TOOL_NAMES]));
    assert.equal(full.type, 'ack');
    assert.deepEqual([...full.tools].sort(), [...PI_TOOL_NAMES].sort());

    const rejections = [
      ['unknown permission', openArgs(`conversation_${randomUUID().replaceAll('-', '')}`, 'new', 'admin')],
      ['empty tool set', openArgs(`conversation_${randomUUID().replaceAll('-', '')}`, 'new', 'read', [])],
      [
        'tool outside the registered surface',
        openArgs(`conversation_${randomUUID().replaceAll('-', '')}`, 'new', 'read', [...readTools, 'tea_plans_approve']),
      ],
      [
        'duplicate tool name',
        openArgs(`conversation_${randomUUID().replaceAll('-', '')}`, 'new', 'read', [...readTools, readTools[0]]),
      ],
      [
        'missing tool set',
        { conversationId: `conversation_${randomUUID().replaceAll('-', '')}`, mode: 'new', permission: 'read' },
      ],
    ];
    for (const [label, payload] of rejections) {
      const rejected = await client.request('conversation.open', payload);
      assert.equal(rejected.type, 'error', label);
      assert.equal(rejected.code, 'INVALID_COMMAND', label);
    }
    assert.equal(client.child.exitCode, null, 'a rejected permission payload must not terminate the Host');

    assert.equal((await client.request('host.shutdown')).type, 'ack');
    assert.equal(await waitForExit(client.child), 0);
  } finally {
    await stopClient(client);
    await rm(agentDir, { recursive: true, force: true });
  }
});
