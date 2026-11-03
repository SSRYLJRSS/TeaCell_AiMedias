#!/usr/bin/env node
/* global process, console */
/**
 * A6-03 证据：随包 Node Host 空闲 working set 采样。
 *
 * 只读测量，不改产品行为。流程与 `probe-agent-host-stage.mjs` 一致（从成功 Tauri 构建
 * 记录读取内容寻址 overlay，把 agent-host/host + 随包 Node 暂存到临时目录，用最小环境
 * 变量启动），差别在于：握手 ready 后在**不发起任何模型请求**的静置状态下，用
 * PowerShell `Get-Process` 采样 OS 层 WorkingSet64，重复 `--samples` 次。
 *
 * 预算口径见 [docs/PERFORMANCE.md](../docs/PERFORMANCE.md#9-20-agent-windows工程预算已独立复核)：
 *   - Node Host 空闲 working set ≤ 192 MiB
 *
 * 输出 JSON 写入 `--out`（默认 build-cache/agent-host-idle-memory.json），只作证据，
 * 不进入运行时路径。构建产物目录固定为仓库根 `build-cache/`（见 .cargo/config.toml）。
 */
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { spawn, spawnSync } from 'node:child_process';
import { clearTimeout, setTimeout } from 'node:timers';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { WINDOWS_VC_RUNTIME_FILES } from './windows-vc-runtime.mjs';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const buildCacheRoot = join(repositoryRoot, 'build-cache');
const overlayRoot = join(repositoryRoot, 'artifacts', 'agent-host-build');
const buildRecordPath = join(overlayRoot, 'last-successful-desktop-build.json');
const target = 'x86_64-pc-windows-msvc';

export const IDLE_WORKING_SET_BUDGET_MIB = 192;
/** 预算：正常退出（仅空闲或已停止推理的 Host）每次 ≤1 秒。 */
export const EXIT_BUDGET_MS = 1000;
/** 预算：Host 新进程 SDK ready 每次 ≤5 秒。 */
export const READY_BUDGET_MS = 5000;

function parseArgs(argv) {
  const options = { samples: 5, idleMs: 4000, out: join(buildCacheRoot, 'agent-host-idle-memory.json') };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--samples' && argv[index + 1]) options.samples = Number(argv[++index]);
    else if (arg === '--idle-ms' && argv[index + 1]) options.idleMs = Number(argv[++index]);
    else if (arg === '--out' && argv[index + 1]) options.out = resolve(argv[++index]);
    else throw new Error(`未知参数：${arg}`);
  }
  assert.ok(Number.isInteger(options.samples) && options.samples >= 1, '--samples 需为正整数');
  assert.ok(Number.isFinite(options.idleMs) && options.idleMs >= 0, '--idle-ms 需为非负数');
  return options;
}

function minimalEnvironment() {
  const env = { PATH: '' };
  for (const key of ['SystemRoot', 'WINDIR', 'TEMP', 'TMP', 'LANG', 'LC_ALL']) {
    if (process.env[key]) env[key] = process.env[key];
  }
  return env;
}

/** 用 PowerShell 读一个进程的 OS 层 working set（字节）。 */
function sampleWorkingSet(pid) {
  const result = spawnSync(
    'powershell',
    [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      `$p = Get-Process -Id ${pid} -ErrorAction Stop; $p.Refresh(); Write-Output $p.WorkingSet64`,
    ],
    { encoding: 'utf8', windowsHide: true },
  );
  assert.equal(result.status, 0, `Get-Process 失败：${result.stderr}`);
  return Number(result.stdout.trim());
}

async function stageRuntime() {
  const buildRecord = JSON.parse(await readFile(buildRecordPath, 'utf8'));
  assert.equal(buildRecord.schemaVersion, 1, '需要成功桌面构建记录');
  assert.equal(buildRecord.target, target, '成功构建必须是 Windows x64');
  const overlayPath = resolve(buildRecord.overlay);
  assert.ok(
    overlayPath.startsWith(`${overlayRoot}${process.platform === 'win32' ? '\\' : '/'}`),
    '构建记录 overlay 必须位于生成目录内',
  );
  const overlayName = overlayPath.slice(overlayRoot.length + 1);
  const overlayText = await readFile(overlayPath, 'utf8');
  const overlaySha256 = createHash('sha256').update(overlayText).digest('hex');
  assert.equal(overlayName, `tauri-overlay-${overlaySha256.slice(0, 16)}.json`, 'overlay 必须内容寻址一致');
  const overlay = JSON.parse(overlayText);
  const resources = overlay.bundle?.resources;
  assert.ok(resources && typeof resources === 'object', 'overlay 必须含资源映射');
  const appBuild = JSON.parse(await readFile(join(repositoryRoot, 'package.json'), 'utf8')).version;

  const sourceFor = (destination) => {
    const match = Object.entries(resources).find(([, mapped]) => mapped === destination);
    assert.ok(match, `overlay 缺少 ${destination}`);
    return resolve(match[0]);
  };

  const stageRoot = join(buildCacheRoot, `agent-host-idle-stage-${process.pid}-${Date.now()}`);
  const stagedHost = join(stageRoot, 'agent-host', 'host');
  const stagedNode = join(stageRoot, 'agent-host', 'runtime', 'node-v24.19.0-win-x64', 'node.exe');
  await cp(sourceFor('agent-host/host'), stagedHost, { recursive: true, errorOnExist: true, force: false });
  await mkdir(dirname(stagedNode), { recursive: true });
  await cp(sourceFor('agent-host/runtime/node-v24.19.0-win-x64/node.exe'), stagedNode, {
    errorOnExist: true,
    force: false,
  });
  let vcRuntimeBytes = 0;
  for (const name of WINDOWS_VC_RUNTIME_FILES) {
    await cp(sourceFor(name), join(stageRoot, name), { errorOnExist: true, force: false });
    vcRuntimeBytes += (await stat(join(stageRoot, name))).size;
  }
  return { stageRoot, stagedHost, stagedNode, overlayName, overlaySha256, vcRuntimeBytes, appBuild };
}

async function measureOne(options, staged) {
  const { FrameDecoder, encodeFrame, SCHEMA_VERSION, SDK_VERSION } = await import(
    pathToFileURL(join(staged.stagedHost, 'src', 'protocol.mjs')).href
  );
  const agentDir = await mkdtemp(join(tmpdir(), 'teacell-agent-idle-'));
  const instance = randomUUID();
  const child = spawn(
    staged.stagedNode,
    [join(staged.stagedHost, 'src', 'bootstrap.mjs'), agentDir, instance, staged.appBuild, 'fresh'],
    { cwd: agentDir, env: minimalEnvironment(), windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] },
  );
  const decoder = new FrameDecoder();
  const frames = [];
  const waiters = [];
  let stderr = '';
  child.stdout.on('data', (chunk) => {
    for (const frame of decoder.push(chunk)) {
      frames.push(frame);
      if (frame.type === 'session.pin_request') {
        child.stdin.write(
          encodeFrame({ protocol: 1, instance, type: 'session.pin_result', pinId: frame.pinId, approved: true }),
        );
      }
      for (const wake of waiters.splice(0)) wake();
    }
  });
  child.stdout.on('end', () => decoder.end());
  child.stderr.setEncoding('utf8').on('data', (chunk) => {
    stderr = `${stderr}${chunk}`.slice(-4096);
  });

  const waitFor = async (predicate, timeoutMs = 15000) => {
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const index = frames.findIndex(predicate);
      if (index >= 0) return frames.splice(index, 1)[0];
      const remaining = deadline - Date.now();
      assert.ok(remaining > 0, `Host 等待超时；stderr=${stderr}；frames=${JSON.stringify(frames)}`);
      await new Promise((resolveWait, rejectWait) => {
        const wake = () => {
          clearTimeout(timer);
          resolveWait();
        };
        const timer = setTimeout(() => {
          const at = waiters.indexOf(wake);
          if (at >= 0) waiters.splice(at, 1);
          rejectWait(new Error(`Host 等待超时；stderr=${stderr}`));
        }, remaining);
        waiters.push(wake);
      });
    }
  };

  let exitCode;
  const exit = new Promise((resolveExit) => child.once('exit', (code) => {
    exitCode = code;
    resolveExit(code);
  }));

  try {
    child.stdin.write('start\n');
    const readyRequest = `request_${randomUUID().replaceAll('-', '')}`;
    child.stdin.write(
      encodeFrame({
        protocol: 1,
        instance,
        requestId: readyRequest,
        type: 'host.hello',
        appBuild: staged.appBuild,
        schemaVersion: SCHEMA_VERSION,
        sdkVersion: SDK_VERSION,
      }),
    );
    const readyAt = Date.now();
    await waitFor((frame) => frame.requestId === readyRequest && frame.type === 'ready');
    const readyMs = Date.now() - readyAt;
    assert.ok(readyMs <= READY_BUDGET_MS, `Host SDK ready 超预算：${readyMs}ms > ${READY_BUDGET_MS}ms`);
    assert.ok(child.pid, 'Host 必须有 pid');
    // 空闲静置：不发起任何模型请求。
    await new Promise((resolveWait) => setTimeout(resolveWait, options.idleMs));
    const workingSetBytes = sampleWorkingSet(child.pid);
    const shutdownRequest = `request_${randomUUID().replaceAll('-', '')}`;
    const shutdownAt = Date.now();
    child.stdin.write(
      encodeFrame({ protocol: 1, instance, requestId: shutdownRequest, type: 'host.shutdown' }),
    );
    await waitFor((frame) => frame.requestId === shutdownRequest && frame.type === 'ack');
    const closed = await Promise.race([exit, new Promise((resolveWait) => setTimeout(() => resolveWait(-1), 8000))]);
    const exitMs = Date.now() - shutdownAt;
    assert.equal(closed, 0, `Host 未干净退出：code=${closed}`);
    // 预算「正常退出 ≤1 秒」只看空闲 Host 从 shutdown 请求到进程真正退出的墙钟时间。
    assert.ok(exitMs <= EXIT_BUDGET_MS, `Host 空闲退出超预算：${exitMs}ms > ${EXIT_BUDGET_MS}ms`);
    return { readyMs, exitMs, workingSetBytes, workingSetMiB: workingSetBytes / (1024 * 1024), exitCode };
  } finally {
    if (child.exitCode === null) {
      child.kill();
      await Promise.race([exit, new Promise((resolveWait) => setTimeout(resolveWait, 5000))]);
    }
    await rm(agentDir, { recursive: true, force: true });
  }
}

async function main() {
  if (process.platform !== 'win32') {
    throw new Error('该测量脚本只支持 Windows（依赖 Get-Process WorkingSet64）');
  }
  const options = parseArgs(process.argv.slice(2));
  await mkdir(buildCacheRoot, { recursive: true });
  const staged = await stageRuntime();
  const samples = [];
  try {
    for (let index = 0; index < options.samples; index += 1) {
      const sample = await measureOne(options, staged);
      samples.push(sample);
      console.log(
        `[idle-memory] 样本 ${index + 1}/${options.samples}: ${sample.workingSetMiB.toFixed(2)} MiB（ready ${sample.readyMs}ms）`,
      );
    }
  } finally {
    await rm(staged.stageRoot, { recursive: true, force: true });
  }
  const maxWorkingSetBytes = Math.max(...samples.map((sample) => sample.workingSetBytes));
  const maxWorkingSetMiB = maxWorkingSetBytes / (1024 * 1024);
  const readyValues = samples.map((sample) => sample.readyMs);
  const exitValues = samples.map((sample) => sample.exitMs);
  const maxReadyMs = Math.max(...readyValues);
  const maxExitMs = Math.max(...exitValues);
  const payload = {
    schemaVersion: 1,
    measuredAt: new Date().toISOString(),
    platform: process.platform,
    arch: process.arch,
    nodeVersion: 'v24.19.0',
    overlay: staged.overlayName,
    overlaySha256: staged.overlaySha256,
    vcRuntimeBytes: staged.vcRuntimeBytes,
    appBuild: staged.appBuild,
    mode: 'staged production Host, idle (no model request), OS WorkingSet64 via Get-Process',
    samples,
    maxWorkingSetBytes,
    maxWorkingSetMiB: Number(maxWorkingSetMiB.toFixed(3)),
    budgetMiB: IDLE_WORKING_SET_BUDGET_MIB,
    withinBudget: maxWorkingSetMiB <= IDLE_WORKING_SET_BUDGET_MIB,
    readyBudgetMs: READY_BUDGET_MS,
    maxReadyMs,
    readyWithinBudget: maxReadyMs <= READY_BUDGET_MS,
    exitBudgetMs: EXIT_BUDGET_MS,
    maxExitMs,
    exitWithinBudget: maxExitMs <= EXIT_BUDGET_MS,
  };
  await writeFile(options.out, `${JSON.stringify(payload, null, 2)}\n`, 'utf8');
  console.log(`[idle-memory] 峰值 ${maxWorkingSetMiB.toFixed(2)} MiB / 预算 ${IDLE_WORKING_SET_BUDGET_MIB} MiB → ${payload.withinBudget ? 'PASS' : 'FAIL'}`);
  console.log(`[idle-memory] SDK ready 最大 ${maxReadyMs}ms / 预算 ${READY_BUDGET_MS}ms → ${payload.readyWithinBudget ? 'PASS' : 'FAIL'}`);
  console.log(`[idle-memory] 空闲退出最大 ${maxExitMs}ms / 预算 ${EXIT_BUDGET_MS}ms → ${payload.exitWithinBudget ? 'PASS' : 'FAIL'}`);
  console.log(`[idle-memory] 证据：${options.out}`);
  if (!payload.withinBudget || !payload.readyWithinBudget || !payload.exitWithinBudget) process.exitCode = 1;
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try {
    await main();
  } catch (error) {
    console.error(`[idle-memory] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
