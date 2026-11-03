/*
 * A5-03 两个真实客户端脚本的契约测试。
 *
 * 真正的端到端互操作由 Rust 集成测试 `src-tauri/tests/mcp_interop.rs` 驱动：
 * 它需要一个真宿主、真配对、以及必须插在两次调用之间的 GUI 审批。
 *
 * 这里只锁住客户端脚本自身的诚实性 —— 这些性质与宿主无关，
 * 一旦退化成「静默失败」或「多行输出」，Rust 侧会因为解析不到报告而误判：
 *   1. 无论成败都只往 stdout 写**一行** JSON；
 *   2. 失败必须带可判定的 code，不能崩栈或写空；
 *   3. 未知参数必须被拒绝，不能悄悄忽略（否则剧本会跑错步骤还显示成功）。
 */
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(here, '..');
const nodeClient = path.join(packageRoot, 'client-node.mjs');
const pythonClient = path.join(packageRoot, 'client-python.py');

/** 不存在的端点：客户端必须快速失败而不是挂死等握手。 */
const missingEndpoint = path.join(packageRoot, 'test', 'no-such-endpoint.json');

function readReport(stdout, label) {
  const lines = stdout.split('\n').filter(line => line.trim().length > 0);
  assert.equal(lines.length, 1, `${label} 必须只输出一行 JSON，实际：${stdout}`);
  return JSON.parse(lines[0]);
}

function findPython() {
  const candidates = [
    process.env.TEACELL_INTEROP_PYTHON,
    'C:\\Users\\33887\\AppData\\Local\\Programs\\Python\\Python313\\python.exe',
    'python',
    'python3',
  ].filter(Boolean);
  for (const candidate of candidates) {
    if (candidate.includes('\\') && !existsSync(candidate)) continue;
    const probe = spawnSync(candidate, ['-c', 'import mcp'], { encoding: 'utf8' });
    if (probe.status === 0) return candidate;
  }
  return null;
}

test('node client reports an unreachable host as one JSON line with a code', () => {
  const result = spawnSync(
    process.execPath,
    [
      nodeClient,
      '--mode',
      'discover',
      '--endpoint',
      missingEndpoint,
      '--connection',
      'mcpconn_0123456789abcdef',
      '--bridge',
      path.join(packageRoot, '..', 'mcp-bridge', 'src', 'server.mjs'),
      '--node',
      process.execPath,
    ],
    { encoding: 'utf8', timeout: 60_000 },
  );
  const report = readReport(result.stdout, 'node 客户端');
  // 客户端自身仍然成功启动（MCP 服务起来了），只是工具表为空：
  // 宿主不可用不等于客户端不可用，两者必须能被区分。
  assert.equal(report.ok, true);
  assert.equal(report.toolCount, 0);
  assert.equal(report.client, 'node:@modelcontextprotocol/sdk');
});

test('node client refuses unknown parameters instead of ignoring them', () => {
  const result = spawnSync(
    process.execPath,
    [nodeClient, '--mode', 'discover', '--endpoint', missingEndpoint, '--bogus', 'x'],
    { encoding: 'utf8', timeout: 60_000 },
  );
  const report = readReport(result.stdout, 'node 客户端');
  assert.equal(report.ok, false);
  assert.match(report.code, /^CLIENT_FAILURE$/u);
  assert.notEqual(result.status, 0, '契约错误应以非零退出码结束');
});

test('node client refuses a submit without a plan id', () => {
  const result = spawnSync(
    process.execPath,
    [
      nodeClient,
      '--mode',
      'submit',
      '--endpoint',
      missingEndpoint,
      '--connection',
      'mcpconn_0123456789abcdef',
      '--bridge',
      path.join(packageRoot, '..', 'mcp-bridge', 'src', 'server.mjs'),
      '--node',
      process.execPath,
    ],
    { encoding: 'utf8', timeout: 60_000 },
  );
  const report = readReport(result.stdout, 'node 客户端');
  assert.equal(report.ok, false);
  assert.match(report.message, /plan-id/u);
});

test('python client honours the same CLI surface', t => {
  const python = findPython();
  if (!python) {
    t.skip('本机没有可用的 Python mcp 包，跳过（Rust 集成测试里同样会跳过）');
    return;
  }
  const bridge = path.join(packageRoot, '..', 'mcp-bridge', 'src', 'server.mjs');
  const discover = spawnSync(
    python,
    [
      pythonClient,
      '--mode',
      'discover',
      '--endpoint',
      missingEndpoint,
      '--connection',
      'mcpconn_0123456789abcdef',
      '--bridge',
      bridge,
      '--node',
      process.execPath,
    ],
    { encoding: 'utf8', timeout: 120_000 },
  );
  const report = readReport(discover.stdout, 'python 客户端');
  assert.equal(report.ok, true);
  assert.equal(report.toolCount, 0);
  assert.equal(report.client, 'python:mcp');

  const unknown = spawnSync(
    python,
    [pythonClient, '--mode', 'discover', '--endpoint', missingEndpoint, '--bogus', 'x'],
    { encoding: 'utf8', timeout: 120_000 },
  );
  const refused = readReport(unknown.stdout, 'python 客户端');
  assert.equal(refused.ok, false);
  assert.equal(refused.code, 'CLIENT_FAILURE');
  assert.notEqual(unknown.status, 0, '契约错误应以非零退出码结束');
});
