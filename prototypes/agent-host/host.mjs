import process from 'node:process';
import { mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import { FrameDecoder, encodeFrame, PROTOCOL_VERSION } from './protocol.mjs';
import { createPrototypeSession, SDK_VERSION } from './sdk-session.mjs';
import { acquireLease } from './lease.mjs';

const [directoryArg, instance, recovery] = process.argv.slice(2);
if (!directoryArg || !instance) process.exit(2);
const directory = resolve(directoryArg);
let session;
let activeRun;
let runId;
let sequence = 0;
let shuttingDown = false;
let releaseLease;
const send = payload => {
  const frame = encodeFrame({ protocol: PROTOCOL_VERSION, instance, sequence: ++sequence, ...payload });
  // This PoC stops on backpressure instead of growing a queue indefinitely.
  if (!process.stdout.write(frame)) void shutdown(4);
};

async function shutdown(code = 0) {
  if (shuttingDown) return;
  shuttingDown = true;
  process.stdin.pause();
  await session?.abort();
  await activeRun?.catch(() => {});
  session?.dispose();
  await releaseLease?.();
  process.exitCode = code;
  process.stdin.destroy();
}

async function handle(frame) {
  if (frame.protocol !== PROTOCOL_VERSION || frame.instance !== instance) {
    throw new Error('IDENTITY_OR_VERSION_MISMATCH');
  }
  if (frame.type === 'shutdown') return shutdown();
  if (frame.type === 'measure') {
    send({ type: 'measurement', memory: process.memoryUsage(), cpu: process.cpuUsage() });
    return;
  }
  if (frame.type === 'stop') {
    if (frame.runId !== runId) throw new Error('RUN_MISMATCH');
    await session.abort();
    return;
  }
  if (frame.type !== 'prompt' || typeof frame.text !== 'string' ||
      frame.text.length > 4096 || typeof frame.runId !== 'string' ||
      frame.runId.length > 128) throw new Error('INVALID_COMMAND');
  if (activeRun) throw new Error('RUN_BUSY');
  runId = frame.runId;
  activeRun = session.prompt(frame.text);
  try {
    await activeRun;
    send({ type: 'settled', runId,
      aborted: session.agent.state.messages.at(-1)?.stopReason === 'aborted' });
  } finally {
    activeRun = undefined;
  }
}

try {
  await mkdir(directory, { recursive: true });
  releaseLease = await acquireLease(directory, instance, recovery === '--recover-stranded');
  session = await createPrototypeSession(directory);
  session.subscribe(event => {
    if (event.type === 'message_update' && event.assistantMessageEvent.type === 'text_delta') {
      send({ type: 'delta', runId, text: event.assistantMessageEvent.delta });
    }
  });
  const decoder = new FrameDecoder();
  process.stdin.on('data', chunk => {
    try {
      for (const frame of decoder.push(chunk)) {
        void handle(frame).catch(() => shutdown(3));
      }
    } catch { void shutdown(3); }
  });
  process.stdin.on('end', () => { void shutdown(); });
  process.stdin.on('error', () => { void shutdown(3); });
  process.stdout.on('error', () => { void shutdown(3); });
  process.on('SIGTERM', () => { void shutdown(); });
  process.stdin.resume();
  send({ type: 'ready', sdk: SDK_VERSION, tools: session.getActiveToolNames() });
} catch {
  // Do not serialize exceptions: loader/provider errors can contain sensitive input.
  await shutdown(2);
}
