#!/usr/bin/env node
/*
 * A5-03 真实 MCP 客户端（Node）。
 *
 * 用官方 @modelcontextprotocol/sdk 走 stdio 连到 packages/mcp-bridge，
 * 桥接再用本机命名管道连到 Rust 宿主。这个脚本本身不碰数据库、不碰文件，
 * 只扮演「外部客户端」。
 *
 * 每次调用只做一件事，把结果作为单行 JSON 写到 stdout：
 * 真正的生产方是 Rust 集成测试，它需要在两次调用之间插入 GUI 审批，
 * 所以客户端不能自己一口气跑完整个剧本。
 *
 * 用法：
 *   node client-node.mjs --mode discover --endpoint <path> --connection <id> --bridge <path>
 *   node client-node.mjs --mode prepare  ... 
 *   node client-node.mjs --mode submit   ... --plan-id <id> [--idempotency-key <k>]
 *   node client-node.mjs --mode submit-unapproved ... --plan-id <id>
 *   node client-node.mjs --mode status   ... --operation-id <id>
 */
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';

const PARAMETERS = {
  mode: 'mode',
  endpoint: 'endpoint',
  connection: 'connection',
  bridge: 'bridge',
  node: 'node',
  'plan-id': 'planId',
  'idempotency-key': 'idempotencyKey',
  'operation-id': 'operationId',
};

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (!flag.startsWith('--')) throw new Error(`无法识别的位置参数：${flag}`);
    const key = PARAMETERS[flag.slice(2)];
    if (!key) throw new Error(`未知参数：${flag}`);
    const value = argv[index + 1];
    if (value === undefined) throw new Error(`参数缺少值：${flag}`);
    options[key] = value;
    index += 1;
  }
  for (const required of ['mode', 'endpoint', 'connection', 'bridge', 'node']) {
    if (!options[required]) throw new Error(`缺少必需参数：--${required}`);
  }
  return options;
}

/** 桥接把工具结果包成 content[0].text；错误则是 "CODE: message"。 */
function unwrap(result) {
  const text = result?.content?.[0]?.text;
  if (typeof text !== 'string') {
    throw new Error(`工具返回缺少文本内容：${JSON.stringify(result)}`);
  }
  if (result.isError) {
    const separator = text.indexOf(':');
    const code = separator === -1 ? text.trim() : text.slice(0, separator).trim();
    const message = separator === -1 ? '' : text.slice(separator + 1).trim();
    const error = new Error(message || text);
    error.mcpCode = code;
    throw error;
  }
  return JSON.parse(text);
}

async function callTool(client, name, args) {
  return unwrap(await client.callTool({ name, arguments: args }));
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const transport = new StdioClientTransport({
    command: options.node,
    args: [options.bridge],
    env: {
      ...process.env,
      TEACELL_MCP_ENDPOINT: options.endpoint,
      TEACELL_MCP_CONNECTION: options.connection,
    },
    stderr: 'pipe',
  });
  const client = new Client({ name: 'teacell-interop-node', version: '1.0.0' }, { capabilities: {} });
  await client.connect(transport);

  const serverInfo = client.getServerVersion();
  const report = {
    client: 'node:@modelcontextprotocol/sdk',
    server: serverInfo,
  };

  try {
    if (options.mode === 'discover') {
      const listed = await client.listTools();
      report.tools = listed.tools.map(tool => tool.name).sort();
      report.toolCount = listed.tools.length;
    } else if (options.mode === 'prepare') {
      const search = await callTool(client, 'tea_assets_search', { text: '' });
      report.selectionRef = search.selectionRef;
      report.total = search.total;
      const prepared = await callTool(client, 'tea_assets_trash_prepare', {
        selectionRef: search.selectionRef,
      });
      report.planId = prepared.planId;
      report.reviewRef = prepared.reviewRef;
      report.itemCount = prepared.itemCount;
      report.originKind = prepared.originKind;
      report.originLabel = prepared.originLabel ?? null;
      report.summary = prepared.summary;
    } else if (options.mode === 'status') {
      // 全新进程、全新连接、没有任何本地状态：只凭稳定的 operationId 查询。
      if (!options.operationId) throw new Error('status 模式需要 --operation-id');
      const snapshot = await callTool(client, 'tea_operations_get', {
        operationId: options.operationId,
      });
      report.operationId = snapshot.operationId;
      report.state = snapshot.state;
      report.phase = snapshot.phase;
      report.completed = snapshot.completed;
      report.failed = snapshot.failed;
      report.total = snapshot.total ?? null;
      report.canCancel = snapshot.canCancel;
      report.canResume = snapshot.canResume;
    } else if (options.mode === 'submit' || options.mode === 'submit-unapproved') {
      if (!options.planId) throw new Error('submit 模式需要 --plan-id');
      const key = options.idempotencyKey ?? `interop_${options.mode}`;
      const submitted = await callTool(client, 'tea_assets_trash_submit', {
        planId: options.planId,
        idempotencyKey: key,
      });
      report.operationId = submitted.operation.operationId;
      report.replayed = submitted.replayed;
      const snapshot = await callTool(client, 'tea_operations_get', {
        operationId: submitted.operation.operationId,
      });
      report.state = snapshot.state;
      report.total = snapshot.total ?? null;
      report.completed = snapshot.completed;
    } else {
      throw new Error(`未知模式：${options.mode}`);
    }
    report.ok = true;
  } catch (error) {
    report.ok = false;
    report.code = error?.mcpCode ?? error?.code ?? 'UNKNOWN';
    report.message = error?.message ?? String(error);
  } finally {
    await client.close();
  }

  process.stdout.write(`${JSON.stringify(report)}\n`);
}

main().catch(error => {
  process.stdout.write(
    `${JSON.stringify({ ok: false, code: 'CLIENT_FAILURE', message: error?.message ?? String(error) })}\n`,
  );
  process.exitCode = 1;
});
