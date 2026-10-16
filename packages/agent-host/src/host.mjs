import process from 'node:process';
import { Buffer } from 'node:buffer';
import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';
import { FrameDecoder, encodeFrame, MAX_PROMPT_BYTES, PROTOCOL_VERSION, RESOURCE_TOKEN, SCHEMA_VERSION, SDK_VERSION, requireRequestId } from './protocol.mjs';
import { createOfflineSession } from './sdk-session.mjs';
import { createGatewayRpc, gatewayErrorDetails } from './gateway-rpc.mjs';
import { verifyAgentDirectory } from './paths.mjs';
import { acquireLease } from './lease.mjs';
import { createWriteQueue } from './write-queue.mjs';
import { clearTimeout, setTimeout } from 'node:timers';

const [agentDirArg, hostInstanceId, appBuild, recoveryMode] = process.argv.slice(2);
const agentDir = resolve(agentDirArg ?? '');
let sequence = 0;
let shuttingDown = false;
let activeAgentDir;
let releaseLease;
let activeRun;
let gatewayRpc;
const sessions = new Map();
const MAX_OPEN_SESSIONS = 8;
const output = createWriteQueue(process.stdout);
const pendingCommands = new Set();
const pendingSessionPins = new Map();
const SESSION_PIN_TIMEOUT_MS = 2000;
const RECOVERABLE_COMMAND_ERRORS = new Set([
  'INVALID_CONVERSATION_ID', 'INVALID_OPEN_MODE', 'RUN_BUSY', 'CONVERSATION_EXISTS',
  'SESSION_LIMIT', 'SESSION_OPEN_FAILED', 'INVALID_PROMPT', 'INVALID_RUN_ID',
  'CONVERSATION_NOT_OPEN', 'RUN_MISMATCH', 'SESSION_FILE_INVALID', 'INVALID_COMMAND',
  'INVALID_MODEL_LEASE',
]);

class CommandRejection extends Error {
  constructor(code) {
    if (!RECOVERABLE_COMMAND_ERRORS.has(code)) throw new Error('INVALID_RECOVERABLE_ERROR_CODE');
    super(code);
    this.code = code;
  }
}

function rejectCommand(code) {
  throw new CommandRejection(code);
}

function send(payload) {
  if (!Number.isSafeInteger(sequence + 1)) throw new Error('EVENT_SEQUENCE_EXHAUSTED');
  const frame = encodeFrame({ protocol: PROTOCOL_VERSION, instance: hostInstanceId, sequence: ++sequence, ...payload });
  const write = output.write(frame);
  void write.catch(() => { void shutdown(); });
  return write;
}

function ack(requestId, command, extra = {}) {
  send({ type: 'ack', requestId, command, ...extra });
}

function error(requestId, code, runId, conversationId, gatewayError) {
  send({ type: 'error', ...(requestId ? { requestId } : {}), code,
    ...(runId ? { runId } : {}), ...(conversationId ? { conversationId } : {}),
    ...(gatewayError ? { gatewayError } : {}) });
}

async function stopRun() {
  if (!activeRun) return;
  const current = activeRun;
  current.cancelled = true;
  await current.setupPromise.catch(() => {});
  if (current.promptStarted) {
    try { await current.session.abort(); } catch { /* Preserve shutdown progress if abort already failed. */ }
    await current.promise.catch(() => {});
  }
}

async function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  try {
    await stopRun();
    process.stdin.pause();
    gatewayRpc?.close();
    await Promise.allSettled([...pendingCommands]);
    for (const [conversationId, record] of sessions) {
      try { await record.session.dispose(); } catch { /* Continue disposing the remaining sessions. */ }
      if (record.pinnedSessionFile) await unpinSessionFile(conversationId, record.pinnedSessionFile).catch(() => {});
    }
  } finally {
    process.stdin.pause();
    gatewayRpc?.close();
    sessions.clear();
    try { await releaseLease?.(); } catch { /* The process is exiting; do not hold the lease indefinitely. */ }
    try { await output.flush(); } catch { /* A broken output pipe must not block process cleanup. */ }
    process.exitCode = 0;
    process.stdin.destroy();
    process.stdout.end();
  }
}

async function finishRun(run, text) {
  try {
    run.promise = Promise.resolve(run.session.prompt(text));
    await run.promise;
    const gatewayFailure = gatewayRpc?.failureForRun(run.runId);
    if (gatewayFailure) {
      error(undefined, 'RUN_FAILED', run.runId, run.conversationId, gatewayFailure);
    }
    send({ type: 'settled', conversationId: run.conversationId, runId: run.runId,
      aborted: Boolean(gatewayFailure) ||
        run.session.agent.state.messages.at(-1)?.stopReason === 'aborted' });
  } catch (failure) {
    error(undefined, 'RUN_FAILED', run.runId, run.conversationId,
      gatewayErrorDetails(failure) ?? gatewayRpc?.failureForRun(run.runId));
    send({ type: 'settled', conversationId: run.conversationId, runId: run.runId, aborted: true });
  } finally {
    run.clearGatewayLease?.(run.runId);
    if (activeRun === run) activeRun = undefined;
  }
}

function conversationIdOf(value) {
  if (typeof value !== 'string' || !RESOURCE_TOKEN.test(value)) rejectCommand('INVALID_CONVERSATION_ID');
  return value;
}

async function handle(frame) {
  const requestId = requireRequestId(frame.requestId);
  if (frame.protocol !== PROTOCOL_VERSION || frame.instance !== hostInstanceId) throw new Error('IDENTITY_OR_VERSION_MISMATCH');

  if (frame.type === 'host.hello') {
    if (frame.schemaVersion !== SCHEMA_VERSION || frame.appBuild !== appBuild || frame.sdkVersion !== SDK_VERSION) {
      error(requestId, 'VERSION_MISMATCH');
      return;
    }
    send({ type: 'ready', requestId, sdk: SDK_VERSION, appBuild,
      schemaVersion: SCHEMA_VERSION, tools: [], modelNetwork: false, gatewayProtocol: 1 });
    return;
  }

  if (frame.type === 'conversation.open') {
    const conversationId = conversationIdOf(frame.conversationId);
    if (!['new', 'open', 'resume'].includes(frame.mode)) rejectCommand('INVALID_OPEN_MODE');
    if (activeRun) rejectCommand('RUN_BUSY');
    const current = sessions.get(conversationId);
    if (current) {
      if (frame.mode === 'new') rejectCommand('CONVERSATION_EXISTS');
      ack(requestId, frame.type, { conversationId, sessionId: current.session.sessionManager.getSessionId() });
      return;
    }
    if (sessions.size >= MAX_OPEN_SESSIONS) rejectCommand('SESSION_LIMIT');
    let session;
    try {
      session = await createOfflineSession(activeAgentDir, conversationId, frame.mode, {
        gatewayRpc,
        pinSessionFile: fileName => requestSessionPin(conversationId, fileName),
        unpinSessionFile: fileName => unpinSessionFile(conversationId, fileName),
      });
    } catch {
      rejectCommand('SESSION_OPEN_FAILED');
    }
    sessions.set(conversationId, session);
    session.session.subscribe(event => {
      if (event.type !== 'message_update' || event.assistantMessageEvent.type !== 'text_delta') return;
      const run = activeRun;
      if (!run || run.conversationId !== conversationId) return;
      try { send({ type: 'delta', conversationId, runId: run.runId, text: event.assistantMessageEvent.delta }); }
      catch { void shutdown(); }
    });
    ack(requestId, frame.type, { conversationId, sessionId: session.session.sessionManager.getSessionId() });
    return;
  }

  if (frame.type === 'conversation.send') {
    const conversationId = conversationIdOf(frame.conversationId);
    if (typeof frame.text !== 'string' || Buffer.byteLength(frame.text, 'utf8') > MAX_PROMPT_BYTES || frame.text.includes('\0')) {
      rejectCommand('INVALID_PROMPT');
    }
    if (typeof frame.runId !== 'string' || !RESOURCE_TOKEN.test(frame.runId)) rejectCommand('INVALID_RUN_ID');
    if (activeRun) rejectCommand('RUN_BUSY');
    const record = sessions.get(conversationId);
    if (!record) rejectCommand('CONVERSATION_NOT_OPEN');
    const run = {
      conversationId,
      runId: frame.runId,
      session: record.session,
      clearGatewayLease: record.clearGatewayLease,
      promise: Promise.resolve(),
      cancelled: false,
      promptStarted: false,
      setupPromise: Promise.resolve(),
    };
    activeRun = run;
    if (frame.gatewayLease !== undefined) {
      run.setupPromise = record.setGatewayLease(frame.gatewayLease, { conversationId, runId: frame.runId })
        .catch(() => { throw new CommandRejection('INVALID_MODEL_LEASE'); });
    } else {
      record.clearGatewayLease();
    }
    try {
      await run.setupPromise;
    } catch (failure) {
      if (activeRun === run) activeRun = undefined;
      record.clearGatewayLease(frame.runId);
      throw failure;
    }
    if (run.cancelled || activeRun !== run) {
      record.clearGatewayLease(frame.runId);
      rejectCommand('RUN_MISMATCH');
    }
    ack(requestId, frame.type, { conversationId, runId: frame.runId });
    run.promptStarted = true;
    void finishRun(run, frame.text).catch(() => { void shutdown(); });
    return;
  }

  if (frame.type === 'conversation.cancel_run') {
    const conversationId = conversationIdOf(frame.conversationId);
    if (!activeRun || activeRun.conversationId !== conversationId || activeRun.runId !== frame.runId) {
      rejectCommand('RUN_MISMATCH');
    }
    await stopRun();
    ack(requestId, frame.type, { conversationId, runId: frame.runId });
    return;
  }

  if (frame.type === 'conversation.close') {
    const conversationId = conversationIdOf(frame.conversationId);
    if (activeRun?.conversationId === conversationId) await stopRun();
    const record = sessions.get(conversationId);
    record?.clearGatewayLease?.();
    await record?.session.dispose();
    if (record?.pinnedSessionFile) await unpinSessionFile(conversationId, record.pinnedSessionFile);
    sessions.delete(conversationId);
    ack(requestId, frame.type, { conversationId });
    return;
  }

  if (frame.type === 'host.shutdown') {
    ack(requestId, frame.type);
    await shutdown();
    return;
  }

  throw new Error('UNKNOWN_COMMAND');
}

export async function runHost(options = {}) {
  if (!agentDirArg || !hostInstanceId || !appBuild || !['fresh', 'recover-stranded'].includes(recoveryMode)) return 2;
  const directory = await verifyAgentDirectory(resolve(options.agentDir ?? agentDir));
  activeAgentDir = directory;
  releaseLease = await acquireLease(directory, hostInstanceId, recoveryMode === 'recover-stranded');
  gatewayRpc = createGatewayRpc({
    instanceId: hostInstanceId,
    sendFrame: frame => send(frame),
  });
  const decoder = new FrameDecoder();
  process.stdin.on('data', chunk => {
    try {
      for (const frame of decoder.push(chunk)) {
        if (!frame || Array.isArray(frame) || typeof frame !== 'object') throw new Error('INVALID_FRAME');
        if (frame.type === 'session.pin_result') {
          receiveSessionPinResult(frame);
          continue;
        }
        const task = dispatch(frame);
        if (frame.type !== 'host.shutdown') {
          pendingCommands.add(task);
          void task.then(() => pendingCommands.delete(task), () => pendingCommands.delete(task));
        }
      }
    } catch {
      void shutdown();
    }
  });
  process.stdin.on('end', () => { void shutdown(); });
  process.stdin.on('error', () => { void shutdown(); });
  process.stdout.on('error', () => { void shutdown(); });
  process.on('SIGTERM', () => { void shutdown(); });
  process.stdin.resume();
  return 0;
}

async function requestSessionPin(conversationId, fileName) {
  if (!/^[A-Za-z0-9_.-]{1,240}\.jsonl$/u.test(fileName)) throw new Error('SESSION_FILE_INVALID');
  const pinId = `pin_${randomUUID().replaceAll('-', '')}`;
  let resolveResult;
  let rejectResult;
  const result = new Promise((resolveResultPromise, rejectResultPromise) => {
    resolveResult = resolveResultPromise;
    rejectResult = rejectResultPromise;
  });
  const timer = setTimeout(() => {
    pendingSessionPins.delete(pinId);
    rejectResult(new Error('SESSION_PIN_TIMEOUT'));
  }, SESSION_PIN_TIMEOUT_MS);
  pendingSessionPins.set(pinId, { resolve: resolveResult, reject: rejectResult, timer });
  try {
    await send({ type: 'session.pin_request', pinId, conversationId, fileName });
    if (!await result) throw new Error('SESSION_FILE_INVALID');
  } catch (failure) {
    const pending = pendingSessionPins.get(pinId);
    if (pending) {
      pendingSessionPins.delete(pinId);
      clearTimeout(pending.timer);
    }
    throw failure;
  }
}

function receiveSessionPinResult(frame) {
  const common = ['protocol', 'instance', 'type', 'pinId', 'approved'];
  const keys = Object.keys(frame).sort();
  const approved = frame.approved === true;
  const expected = [...common, ...(approved ? [] : ['code'])].sort();
  if (frame.protocol !== PROTOCOL_VERSION || frame.instance !== hostInstanceId ||
      frame.type !== 'session.pin_result' || typeof frame.pinId !== 'string' ||
      !RESOURCE_TOKEN.test(frame.pinId) || typeof frame.approved !== 'boolean' ||
      (!approved && frame.code !== 'SESSION_FILE_INVALID') ||
      keys.length !== expected.length || keys.some((key, index) => key !== expected[index])) {
    throw new Error('INVALID_SESSION_PIN_RESULT');
  }
  const pending = pendingSessionPins.get(frame.pinId);
  if (!pending) return;
  pendingSessionPins.delete(frame.pinId);
  clearTimeout(pending.timer);
  pending.resolve(approved);
}

async function unpinSessionFile(conversationId, fileName) {
  if (!RESOURCE_TOKEN.test(conversationId) || !/^[A-Za-z0-9_.-]{1,240}\.jsonl$/u.test(fileName) ||
      !fileName.endsWith(`_${conversationId}.jsonl`)) return;
  await send({ type: 'session.unpin_request', conversationId, fileName });
}

async function dispatch(frame) {
  if (typeof frame?.type === 'string' && frame.type.startsWith('gateway.')) {
    try {
      gatewayRpc?.receive(frame);
    } catch {
      void shutdown();
    }
    return;
  }
  try {
    await handle(frame);
  } catch (failure) {
    const recoverable = failure instanceof CommandRejection;
    try {
      error(
        typeof frame.requestId === 'string' ? frame.requestId : undefined,
        recoverable ? failure.code : 'INVALID_COMMAND',
      );
    } catch {
      void shutdown();
      return;
    }
    if (!recoverable) void shutdown();
  }
}

if (import.meta.url === `file://${process.argv[1]?.replaceAll('\\', '/')}`) {
  runHost().then(code => { if (code) process.exitCode = code; }).catch(() => {
    try { error(undefined, 'HOST_START_FAILED'); } catch { /* Startup failure still exits below. */ }
    process.exitCode = 2;
    process.stdin.destroy();
  });
}
