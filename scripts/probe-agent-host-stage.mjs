#!/usr/bin/env node
/* global process, console */
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { spawn, spawnSync } from 'node:child_process';
import { clearTimeout, setTimeout } from 'node:timers';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { WINDOWS_VC_RUNTIME_FILES } from './windows-vc-runtime.mjs';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const artifactsRoot = join(repositoryRoot, 'artifacts', 'agent-runtime');
const overlayRoot = join(repositoryRoot, 'artifacts', 'agent-host-build');
const buildRecordPath = join(overlayRoot, 'last-successful-desktop-build.json');
const target = 'x86_64-pc-windows-msvc';
const appBuild = JSON.parse(await readFile(join(repositoryRoot, 'package.json'), 'utf8')).version;
const buildRecord = JSON.parse(await readFile(buildRecordPath, 'utf8'));
assert.equal(buildRecord.schemaVersion, 1, 'a successful desktop build record is required');
assert.equal(buildRecord.target, target, 'the successful desktop build must target Windows x64');
const overlayPath = resolve(buildRecord.overlay);
assert.ok(overlayPath.startsWith(`${overlayRoot}${process.platform === 'win32' ? '\\' : '/'}`), 'build record overlay must be inside the generated overlay directory');
assert.match(overlayPath, /tauri-overlay-[a-f0-9]+\.json$/u);
const overlayName = overlayPath.slice(overlayRoot.length + 1);
const overlayText = await readFile(overlayPath, 'utf8');
const overlaySha256 = createHash('sha256').update(overlayText).digest('hex');
assert.equal(overlayName, `tauri-overlay-${overlaySha256.slice(0, 16)}.json`, 'build record must point to the exact content-addressed overlay');
const overlay = JSON.parse(overlayText);
const resources = overlay.bundle?.resources;
assert.ok(resources && typeof resources === 'object', 'overlay must include a resource map');

function sourceFor(destination) {
  const match = Object.entries(resources).find(([, mapped]) => mapped === destination);
  assert.ok(match, `overlay is missing ${destination}`);
  return resolve(match[0]);
}

const hostSource = sourceFor('agent-host/host');
const nodeSource = sourceFor(`agent-host/runtime/node-v24.19.0-win-x64/node.exe`);
const licenseSource = sourceFor('agent-host/runtime/node-v24.19.0-win-x64/LICENSE');
const stageRoot = join(artifactsRoot, `agent-host-stage-${process.pid}-${Date.now()}`);
const stagedHost = join(stageRoot, 'agent-host', 'host');
const stagedNode = join(stageRoot, 'agent-host', 'runtime', 'node-v24.19.0-win-x64', 'node.exe');
const stagedLicense = join(stageRoot, 'agent-host', 'runtime', 'node-v24.19.0-win-x64', 'LICENSE');

await cp(hostSource, stagedHost, { recursive: true, errorOnExist: true, force: false });
await mkdir(dirname(stagedNode), { recursive: true });
await cp(nodeSource, stagedNode, { errorOnExist: true, force: false });
await cp(licenseSource, stagedLicense, { errorOnExist: true, force: false });
let vcRuntimeBytes = 0;
for (const name of WINDOWS_VC_RUNTIME_FILES) {
  const source = sourceFor(name);
  const destination = join(stageRoot, name);
  await cp(source, destination, { errorOnExist: true, force: false });
  vcRuntimeBytes += (await stat(destination)).size;
}

const { FrameDecoder, encodeFrame, SDK_VERSION } = await import(
  pathToFileURL(join(stagedHost, 'src', 'protocol.mjs')).href
);
const runtimeVersion = spawnSync(stagedNode, ['--version'], {
  encoding: 'utf8',
  env: minimalEnvironment(),
  windowsHide: true,
});
assert.equal(runtimeVersion.status, 0, runtimeVersion.stderr);
assert.equal(runtimeVersion.stdout.trim(), 'v24.19.0');

function minimalEnvironment() {
  const env = { PATH: '' };
  for (const key of ['SystemRoot', 'WINDIR', 'TEMP', 'TMP', 'LANG', 'LC_ALL']) {
    if (process.env[key]) env[key] = process.env[key];
  }
  return env;
}

const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-stage-probe-'));
const instance = randomUUID();
const conversationId = `conversation_${randomUUID().replaceAll('-', '')}`;
const runId = `run_${randomUUID().replaceAll('-', '')}`;
const child = spawn(stagedNode, [
  join(stagedHost, 'src', 'bootstrap.mjs'),
  agentDir,
  instance,
  appBuild,
  'fresh',
], {
  cwd: agentDir,
  env: minimalEnvironment(),
  windowsHide: true,
  stdio: ['pipe', 'pipe', 'pipe'],
});
const decoder = new FrameDecoder();
const frames = [];
const waiters = [];
let stderr = '';
let pinCount = 0;
let unpinCount = 0;
child.stdout.on('data', chunk => {
  for (const frame of decoder.push(chunk)) {
    frames.push(frame);
    if (frame.type === 'session.pin_request') {
      pinCount += 1;
      assert.ok(frame.fileName.endsWith(`_${frame.conversationId}.jsonl`));
      child.stdin.write(encodeFrame({
        protocol: 1,
        instance,
        type: 'session.pin_result',
        pinId: frame.pinId,
        approved: true,
      }));
    }
    if (frame.type === 'session.unpin_request') unpinCount += 1;
    for (const wake of waiters.splice(0)) wake();
  }
});
child.stdout.on('end', () => decoder.end());
child.stderr.setEncoding('utf8').on('data', chunk => {
  stderr = `${stderr}${chunk}`.slice(-4096);
});

function send(type, payload = {}) {
  const requestId = `request_${randomUUID().replaceAll('-', '')}`;
  child.stdin.write(encodeFrame({ protocol: 1, instance, requestId, type, ...payload }));
  return requestId;
}

async function waitFor(predicate, timeoutMs = 10000) {
  const deadline = Date.now() + timeoutMs;
  while (true) {
    const index = frames.findIndex(predicate);
    if (index >= 0) return frames.splice(index, 1)[0];
    const remaining = deadline - Date.now();
    assert.ok(remaining > 0, `staged Host timed out; stderr=${stderr}; frames=${JSON.stringify(frames)}`);
    await new Promise((resolveWait, rejectWait) => {
      const wake = () => {
        clearTimeout(timer);
        resolveWait();
      };
      const timer = setTimeout(() => {
        const index = waiters.indexOf(wake);
        if (index >= 0) waiters.splice(index, 1);
        rejectWait(new Error(`staged Host timed out; stderr=${stderr}`));
      }, remaining);
      waiters.push(wake);
    });
  }
}

async function request(type, payload = {}) {
  const requestId = send(type, payload);
  return waitFor(frame => frame.requestId === requestId && ['ack', 'ready', 'error'].includes(frame.type));
}

let exitCode;
const exit = new Promise(resolveExit => child.once('exit', code => {
  exitCode = code;
  resolveExit(code);
}));
try {
  child.stdin.write('start\n');
  const readyRequest = send('host.hello', { appBuild, schemaVersion: 1, sdkVersion: SDK_VERSION });
  const ready = await waitFor(frame => frame.requestId === readyRequest && frame.type === 'ready');
  assert.equal(ready.modelNetwork, false);
  assert.deepEqual(ready.tools, []);

  const opened = await request('conversation.open', { conversationId, mode: 'new' });
  assert.equal(opened.type, 'ack');
  assert.equal(opened.conversationId, conversationId);

  const sendRequest = send('conversation.send', { conversationId, runId, text: 'staged production probe' });
  assert.equal((await waitFor(frame => frame.requestId === sendRequest && frame.type === 'ack')).runId, runId);
  assert.ok((await waitFor(frame => frame.type === 'delta' && frame.runId === runId)).text.length > 0);
  assert.equal((await waitFor(frame => frame.type === 'settled' && frame.runId === runId)).aborted, false);

  assert.equal((await request('conversation.close', { conversationId })).type, 'ack');
  const shutdown = await request('host.shutdown');
  assert.equal(shutdown.type, 'ack');
  assert.equal(await Promise.race([exit, new Promise(resolveWait => setTimeout(() => resolveWait(-1), 5000))]), 0);
  assert.equal(exitCode, 0);
  assert.equal(pinCount, 1);
  assert.equal(unpinCount, 1);
  console.log(JSON.stringify({
    result: 'passed',
    target,
    nodeVersion: runtimeVersion.stdout.trim(),
    vcRuntimeFiles: WINDOWS_VC_RUNTIME_FILES.length,
    vcRuntimeBytes,
    overlay: overlayName,
    overlaySha256,
    stageRoot,
    stagedHost: 'agent-host/host',
    pinCount,
    unpinCount,
  }, null, 2));
} finally {
  if (child.exitCode === null) {
    child.kill();
    await Promise.race([exit, new Promise(resolveWait => setTimeout(resolveWait, 5000))]);
  }
  await rm(agentDir, { recursive: true, force: true });
}
