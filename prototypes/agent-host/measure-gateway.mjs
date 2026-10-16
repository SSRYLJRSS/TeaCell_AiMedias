// Windows-only controlled mock measurement; no user service or media access.
import process from 'node:process';
import { spawn } from 'node:child_process';
import { mkdir, writeFile, readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { join, dirname } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import { setInterval, clearInterval, setTimeout, clearTimeout } from 'node:timers';
import { fixture, executable } from './test/helpers/gateway-fixture.mjs';
import { createGatewaySession } from './gateway-session.mjs';
import { childEnvironment } from './supervisor.mjs';
import { performance } from 'node:perf_hooks';
import { Buffer } from 'node:buffer';

if (process.platform !== 'win32') throw new Error('WINDOWS_MEASUREMENT_ONLY');
const output = fileURLToPath(new URL('../../artifacts/agent-runtime/gateway-measurement.json', import.meta.url));
const powershell = join(process.env.SystemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
async function snapshot(gatewayPid) {
  const ids = [process.pid, gatewayPid];
  if (!ids.every(id => Number.isSafeInteger(id) && id > 0)) throw new Error('INVALID_PROCESS_ID');
  const command = `Get-Process -Id ${ids.join(',')} | Select-Object Id,ProcessName,WorkingSet64,PeakWorkingSet64,PrivateMemorySize64,CPU | ConvertTo-Json -Compress`;
  const native = await new Promise((resolve, reject) => {
    const child = spawn(powershell, ['-NoProfile', '-NonInteractive', '-Command', command], {
      env: childEnvironment(process.env), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = ''; let stderr = '';
    const deadline = setTimeout(() => { child.kill(); reject(new Error('MEASUREMENT_TIMEOUT')); }, 5000);
    child.once('error', reject);
    child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
    child.stdout.on('data', chunk => { stdout += chunk; if (stdout.length > 65536) { child.kill(); reject(new Error('MEASUREMENT_LIMIT')); } });
    child.stderr.on('data', chunk => { stderr += chunk; if (stderr.length > 65536) child.kill(); });
    child.once('close', code => {
      clearTimeout(deadline);
      if (code !== 0) { reject(new Error('PROCESS_MEASUREMENT_FAILED')); return; }
      try { resolve(JSON.parse(stdout)); } catch { reject(new Error('INVALID_MEASUREMENT')); }
    });
  });
  if (!Array.isArray(native) || native.length !== 2 || ids.some(id => !native.some(item => item.Id === id))) throw new Error('MISSING_PROCESS');
  return { timestamp: new Date().toISOString(), node: process.memoryUsage(), processes: native };
}

const samples = [];
const profiles = [
  { id: 'short-text', prompt: '固定 mock 流测量，不使用真实服务' },
  { id: 'large-unicode', prompt: '中文照片🌿\n'.repeat(20000) },
  { id: 'near-upload-limit', prompt: 'x'.repeat(1800000) },
];
const repetitions = 10;
const workload = profiles.flatMap(profile => Array.from({ length: repetitions }, () => profile));
for (const [index, profile] of workload.entries()) {
  const run = index + 1;
  const cleanup = [];
  let release;
  let began;
  const streaming = new Promise(resolve => { began = resolve; });
  let interval;
  try {
    const f = await fixture({ after: callback => cleanup.push(callback) }, (_incoming, response) => {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      const delta = () => response.write(`data: ${JSON.stringify({ id: 'measurement', object: 'chat.completion.chunk', created: 1, model: 'mock', choices: [{ index: 0, delta: { content: 'x'.repeat(128) }, finish_reason: null }] })}\n\n`);
      delta(); began(); interval = setInterval(delta, 25);
      release = () => {
        clearInterval(interval);
        response.write(`data: ${JSON.stringify({ id: 'measurement', object: 'chat.completion.chunk', created: 1, model: 'mock', choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`);
        response.end('data: [DONE]\n\n');
      };
    });
    const session = await createGatewaySession(f.directory, f.bridge);
    cleanup.push(() => session.dispose());
    const idle = await snapshot(f.bridge.child.pid);
    const started = performance.now();
    const cpuStarted = process.cpuUsage();
    const prompt = session.prompt(profile.prompt);
    // Attach rejection handling immediately while taking the snapshot.
    const outcome = prompt.then(() => ({ ok: true }), error => ({ ok: false, error: error.message }));
    await Promise.race([streaming, outcome.then(() => { throw new Error('STREAM_NOT_STARTED'); })]);
    if (!session.isStreaming) throw new Error('STREAM_NOT_ACTIVE');
    const active = await snapshot(f.bridge.child.pid);
    if (!session.isStreaming) throw new Error('STREAM_ENDED_DURING_MEASUREMENT');
    release();
    const result = await outcome;
    if (!result.ok || session.agent.state.messages.at(-1).stopReason !== 'stop') throw new Error('STREAM_MEASUREMENT_FAILED');
    const settled = await snapshot(f.bridge.child.pid);
    samples.push({ run, profile: profile.id, promptUtf8Bytes: Buffer.byteLength(profile.prompt),
      serializedFixtureRequestBytes: Buffer.byteLength(JSON.stringify(f.received[0].body)),
      idle, streaming: active, settled, nodeCpuMicroseconds: process.cpuUsage(cpuStarted),
      promptDurationMs: performance.now() - started, attempts: f.bridge.attempts, chunks: f.bridge.chunks });
  } finally {
    clearInterval(interval);
    for (const callback of cleanup.reverse()) await callback();
  }
}
await mkdir(dirname(output), { recursive: true });
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const build = { gatewayExecutableSha256: digest(await readFile(executable)),
  cargoManifestSha256: digest(await readFile(new URL('../model-gateway/Cargo.toml', import.meta.url))),
  cargoLockSha256: digest(await readFile(new URL('../model-gateway/Cargo.lock', import.meta.url))),
  transportSha256: digest(await readFile(new URL('./gateway-transport.mjs', import.meta.url))),
  requestBodySha256: digest(await readFile(new URL('./request-body.mjs', import.meta.url))),
  sessionSha256: digest(await readFile(new URL('./gateway-session.mjs', import.meta.url))),
  measurementSha256: digest(await readFile(new URL('./measure-gateway.mjs', import.meta.url))) };
await writeFile(output, `${JSON.stringify({ schema: 2, platform: process.platform, arch: process.arch, node: process.version, fixture: true, build,
  workload: { nodeProcess: 'one shared process', gatewayBuild: 'debug',
    sdk: '1.0.4', model: 'mock', protocol: 'openai-completions', tools: 0,
    responseDeltaTextBytes: 128, responseDeltaIntervalMs: 25, concurrentPrompts: 1,
    baseline: 'SDK initialized, before current prompt',
    repetitionsPerProfile: repetitions, order: workload.map(profile => profile.id),
    responseEnd: 'after active process sampling, below 3s HTTP timeout',
    environment: 'No simultaneous test/build command intentionally launched by this runner; other system activity uncontrolled' },
  samples, limitations: ['PowerShell sampling adds load', 'All runs share one Node process; Rust gateway restarts each run',
    'Ten sequential observations per profile are not cold-cache or clean-machine experiments',
    'Actual machine background load uncontrolled; no production workload/accepted budget',
    'Node CPU delta includes fixture server and sampling coordination', 'No real network, media or clean-machine scenario'] }, null, 2)}\n`);
process.stdout.write(`${output}\n`);
