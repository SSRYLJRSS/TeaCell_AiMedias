import test from 'node:test';
import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';

import { FrameDecoder, MAX_FRAME_BYTES } from '../src/protocol.mjs';
import { parseArgs } from '../src/server.mjs';
import {
  TEST_CONNECTION_ID,
  b64,
  captureLogger,
  makeReadyServer,
  makeUnavailableServer,
  parseFrames,
} from './helpers.mjs';

/** Every write to stdout must be one complete JSON object followed by a single LF. */
function assertStdoutIsPureJsonRpc(chunks) {
  for (const chunk of chunks) {
    assert.ok(chunk.endsWith('\n'), `stdout chunk missing LF: ${chunk.slice(0, 40)}`);
    const lines = chunk.split('\n');
    assert.equal(lines.at(-1), '', 'chunk must end with exactly one LF');
    for (const line of lines.slice(0, -1)) {
      assert.ok(line.length > 0, 'stdout must not emit blank lines');
      const parsed = JSON.parse(line);
      assert.equal(typeof parsed, 'object');
      assert.equal(parsed.jsonrpc, '2.0');
    }
  }
}

test('initialize_answers_with_the_frozen_protocol_version', async () => {
  const h = await makeReadyServer();
  await h.server.handleLine(JSON.stringify({
    jsonrpc: '2.0',
    id: 1,
    method: 'initialize',
    params: { protocolVersion: '2026-07-28', capabilities: {}, clientInfo: { name: 'x', version: '1' } },
  }));
  h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }));
  await new Promise((resolve) => setImmediate(resolve));

  const frames = h.frames();
  assert.equal(frames.length, 1, 'notifications must not be answered');
  const init = frames[0];
  assert.equal(init.id, 1);
  assert.equal(init.result.protocolVersion, '2025-11-25');
  assert.deepEqual(init.result.capabilities, { tools: {} });
  assert.deepEqual(init.result.serverInfo, { name: 'teacell-mcp-bridge', version: '1.0.0' });
  assert.equal(h.server.lastProtocolVersion, '2026-07-28');
  assert.equal(h.server.initialized, true);
});

test('initialize_never_advertises_sampling_roots_prompts_or_tasks', async () => {
  const h = await makeReadyServer();
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} }));
  const init = h.frames()[0];
  const keys = Object.keys(init.result.capabilities).sort();
  assert.deepEqual(keys, ['tools']);
  assert.equal(init.result.capabilities.sampling, undefined);
  assert.equal(init.result.capabilities.roots, undefined);
  assert.equal(init.result.capabilities.prompts, undefined);
  assert.equal(init.result.capabilities.tasks, undefined);
});

test('ping_answers_with_an_empty_result', async () => {
  const h = await makeReadyServer();
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 'p1', method: 'ping' }));
  const frames = h.frames();
  assert.equal(frames.length, 1);
  assert.deepEqual(frames[0], { jsonrpc: '2.0', id: 'p1', result: {} });
});

test('tools_list_relays_the_welcome_table_verbatim', async () => {
  const h = await makeReadyServer();
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list' }));
  const frames = h.frames();
  assert.equal(frames.length, 1);
  const listed = frames[0].result.tools;
  assert.deepEqual(listed.map((tool) => tool.name), ['tea_assets_search', 'tea_operations_get']);
  assert.deepEqual(listed, JSON.parse(JSON.stringify(h.bridge.tools)));
  // Approval capabilities are never published.
  for (const tool of listed) {
    assert.ok(!['plans.approve', 'operations.resume'].includes(tool.capability), tool.capability);
    assert.ok(!tool.capability.startsWith('trash.empty'), tool.capability);
  }
});

test('tools_call_relays_the_payload_and_returns_text_content', async () => {
  const h = await makeReadyServer();
  const handled = h.server.handleLine(JSON.stringify({
    jsonrpc: '2.0',
    id: 3,
    method: 'tools/call',
    params: { name: 'tea_assets_search', arguments: { query: '猫', limit: 2 } },
  }));
  await new Promise((resolve) => setImmediate(resolve));

  const call = h.transport.sent[1];
  assert.equal(call.type, 'call');
  assert.equal(call.capability, 'assets.search');
  assert.deepEqual(JSON.parse(Buffer.from(call.payload, 'base64').toString('utf8')), {
    query: '猫',
    limit: 2,
  });

  h.transport.inject({
    type: 'result',
    requestId: call.requestId,
    ok: true,
    payload: b64({ items: [{ assetId: 'asset_1' }], total: 1 }),
  });
  await handled;

  const frames = h.frames();
  assert.equal(frames.length, 1);
  const payload = frames[0].result;
  assert.equal(payload.isError, undefined);
  assert.equal(payload.content.length, 1);
  assert.equal(payload.content[0].type, 'text');
  assert.deepEqual(JSON.parse(payload.content[0].text), { items: [{ assetId: 'asset_1' }], total: 1 });
});

test('tools_call_unknown_tool_is_a_json_rpc_error', async () => {
  const h = await makeReadyServer();
  await h.server.handleLine(JSON.stringify({
    jsonrpc: '2.0',
    id: 4,
    method: 'tools/call',
    params: { name: 'tea_plans_approve', arguments: {} },
  }));
  const frame = h.frames()[0];
  assert.equal(frame.error.code, -32602);
  assert.equal(frame.result, undefined);
  assert.equal(h.transport.sent.length, 1, 'unknown tools must never reach the host');
});

test('tools_call_arguments_must_be_an_object', async () => {
  const h = await makeReadyServer();
  for (const args of [[], 'x', 5, null]) {
    await h.server.handleLine(JSON.stringify({
      jsonrpc: '2.0',
      id: 5,
      method: 'tools/call',
      params: { name: 'tea_assets_search', arguments: args },
    }));
  }
  await h.server.handleLine(JSON.stringify({
    jsonrpc: '2.0',
    id: 6,
    method: 'tools/call',
    params: { arguments: {} },
  }));
  const frames = h.frames();
  assert.equal(frames.length, 5);
  for (const frame of frames) assert.equal(frame.error.code, -32602);
});

test('tools_call_failure_carries_a_contract_error_code_and_is_error', async () => {
  const h = await makeReadyServer();
  const handled = h.server.handleLine(JSON.stringify({
    jsonrpc: '2.0',
    id: 7,
    method: 'tools/call',
    params: { name: 'tea_assets_search', arguments: { query: 'x' } },
  }));
  await new Promise((resolve) => setImmediate(resolve));
  const call = h.transport.sent[1];
  h.transport.inject({
    type: 'result',
    requestId: call.requestId,
    ok: false,
    code: 'NOT_ALLOWED',
    message: '能力超出连接范围',
  });
  await handled;

  const result = h.frames()[0].result;
  assert.equal(result.isError, true);
  assert.equal(result.content[0].type, 'text');
  assert.match(result.content[0].text, /NOT_ALLOWED/u);
});

test('host_unavailable_serves_an_empty_tool_table_and_deterministic_code', async () => {
  const h = await makeUnavailableServer('HOST_UNAVAILABLE');

  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} }));
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list' }));
  await h.server.handleLine(JSON.stringify({
    jsonrpc: '2.0',
    id: 3,
    method: 'tools/call',
    params: { name: 'tea_assets_search', arguments: {} },
  }));

  const frames = h.frames();
  assert.equal(frames.length, 3);
  assert.equal(frames[0].result.protocolVersion, '2025-11-25', 'session must still start');
  assert.deepEqual(frames[1].result.tools, [], 'host down means no tools');
  assert.equal(frames[2].result.isError, true);
  assert.match(frames[2].result.content[0].text, /HOST_UNAVAILABLE/u);
  assert.equal(h.transport.sent.length, 0, 'nothing may be sent to a missing host');
});

test('pairing_revoked_and_protocol_unsupported_are_relayed_verbatim', async () => {
  for (const code of ['PAIRING_REVOKED', 'PROTOCOL_UNSUPPORTED', 'LIBRARY_OFFLINE', 'UNSUPPORTED']) {
    const h = await makeUnavailableServer(code);
    await h.server.handleLine(JSON.stringify({
      jsonrpc: '2.0',
      id: 9,
      method: 'tools/call',
      params: { name: 'tea_assets_search', arguments: {} },
    }));
    const result = h.frames()[0].result;
    assert.equal(result.isError, true);
    assert.match(result.content[0].text, new RegExp(code, 'u'), code);
  }
});

test('stdout_contains_only_json_rpc_frames_when_the_host_fails_mid_session', async () => {
  const h = await makeReadyServer();
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} }));
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list' }));
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 3, method: 'ping' }));

  h.transport.drop({ code: 'HOST_UNAVAILABLE' });
  await h.server.handleLine(JSON.stringify({
    jsonrpc: '2.0',
    id: 4,
    method: 'tools/call',
    params: { name: 'tea_assets_search', arguments: {} },
  }));

  assertStdoutIsPureJsonRpc(h.out);
  const frames = h.frames();
  assert.equal(frames.length, 4);
  assert.equal(frames[3].result.isError, true);
  // Diagnostics went to the injected logger, not stdout.
  assert.ok(h.log.lines.length > 0);
  for (const line of h.log.lines) assert.ok(!h.out.some((chunk) => chunk.includes(line)));
});

test('unknown_methods_and_malformed_input_never_crash', async () => {
  const h = await makeReadyServer();

  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tasks/list' }));
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'sampling/createMessage' }));
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 3, method: 'roots/list' }));
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 4, method: 'prompts/list' }));
  await h.server.handleLine('not json at all');
  await h.server.handleLine('');
  await h.server.handleLine('   ');
  await h.server.handleLine(JSON.stringify([1, 2, 3]));
  await h.server.handleLine(JSON.stringify({ id: 5 }));
  await h.server.handleLine(JSON.stringify({ jsonrpc: '2.0', id: 6, method: 'initialize', params: {} }));

  const frames = h.frames();
  const methodNotFound = frames.filter((frame) => frame.error?.code === -32601);
  assert.equal(methodNotFound.length, 4, 'four unsupported methods');
  const parseErrors = frames.filter((frame) => frame.error?.code === -32700);
  assert.equal(parseErrors.length, 1);
  const invalidRequests = frames.filter((frame) => frame.error?.code === -32600);
  assert.equal(invalidRequests.length, 2);
  assertStdoutIsPureJsonRpc(h.out);
});

test('oversized_and_fragmented_stdin_frames_are_survivable', async () => {
  const h = await makeReadyServer();
  const decoder = new FrameDecoder();

  const huge = Buffer.from(`${'x'.repeat(MAX_FRAME_BYTES + 5)}\n`, 'utf8');
  const big = decoder.push(huge);
  assert.deepEqual(big.errors.map((error) => error.code), ['FRAME_TOO_LARGE']);
  assert.equal(big.frames.length, 0);

  const request = JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'ping' });
  const half = Math.floor(request.length / 2);
  const a = decoder.push(Buffer.from(request.slice(0, half), 'utf8'));
  const b = decoder.push(Buffer.from(`${request.slice(half)}\n`, 'utf8'));
  assert.equal(a.frames.length, 0);
  assert.equal(b.frames.length, 1);
  await h.server.handleMessage(b.frames[0]);
  assert.equal(h.frames()[0].result !== undefined, true);
});

test('parse_args_prefers_flags_and_falls_back_to_env', () => {
  assert.deepEqual(parseArgs([], {}), { endpointPath: null, connectionId: null });
  assert.deepEqual(
    parseArgs([], { TEACELL_MCP_ENDPOINT: 'E', TEACELL_MCP_CONNECTION: 'C' }),
    { endpointPath: 'E', connectionId: 'C' },
  );
  assert.deepEqual(
    parseArgs(['--endpoint', 'E2', '--connection', 'C2'], { TEACELL_MCP_ENDPOINT: 'E' }),
    { endpointPath: 'E2', connectionId: 'C2' },
  );
  assert.deepEqual(parseArgs(['--endpoint=E3'], {}), { endpointPath: 'E3', connectionId: null });
});

test('stream_framing_errors_are_answered_or_discarded_but_never_crash', async () => {
  const h = await makeReadyServer();

  h.server.handleFramingError({ code: 'INVALID_JSON' });
  h.server.handleFramingError({ code: 'INVALID_UTF8' });
  h.server.handleFramingError({ code: 'NOT_AN_OBJECT' });
  h.server.handleFramingError({ code: 'FRAME_TOO_LARGE' });
  h.server.handleFramingError({ code: 'TRUNCATED_FRAME' });
  h.server.handleFramingError({ code: 'EMPTY_FRAME' });

  const frames = h.frames();
  assert.equal(frames.length, 3, 'parse/utf8 -> -32700, non-object -> -32600, oversize/truncated/empty -> silent');
  assert.deepEqual(frames.map((frame) => frame.error.code), [-32700, -32700, -32600]);
  for (const frame of frames) assert.equal(frame.id, null);
});

test('run_assembles_the_process_without_a_host_and_still_answers', async () => {
  const { PassThrough } = await import('node:stream');
  const stdin = new PassThrough();
  const out = [];
  const stdout = { write: (chunk) => out.push(chunk) };
  const log = captureLogger();

  const { run } = await import('../src/server.mjs');
  const { ready } = await run({
    parsed: { endpointPath: 'C:\\missing\\endpoint.json', connectionId: TEST_CONNECTION_ID },
    log,
    stdin,
    stdout: stdout,
  });
  const outcome = await ready;
  assert.deepEqual(outcome, { ok: false, error: { code: 'HOST_UNAVAILABLE' } });

  stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} })}\n`);
  stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list' })}\n`);
  stdin.write('garbage\n');
  await new Promise((resolve) => setImmediate(resolve));
  stdin.end();

  const frames = parseFrames(out);
  const byId = new Map(frames.map((frame) => [String(frame.id), frame]));
  assert.equal(byId.get('1').result.protocolVersion, '2025-11-25');
  assert.deepEqual(byId.get('2').result.tools, []);
  assert.ok(frames.some((frame) => frame.error?.code === -32700), 'malformed line answered');
  assertStdoutIsPureJsonRpc(out);
  assert.ok(log.lines.some((line) => line.includes('HOST_UNAVAILABLE')), 'deterministic diagnostic');
});

test('no_console_log_is_used_anywhere_in_src', async () => {
  const { readdir, readFile } = await import('node:fs/promises');
  const dir = new URL('../src/', import.meta.url);
  const entries = await readdir(dir);
  for (const entry of entries.filter((name) => name.endsWith('.mjs'))) {
    const text = await readFile(new URL(entry, dir), 'utf8');
    assert.ok(!/console\.(log|info|debug)\s*\(/u.test(text), `${entry} uses console.log`);
  }
});

test('package_has_no_third_party_dependencies', async () => {
  const { readFile } = await import('node:fs/promises');
  const raw = await readFile(new URL('../package.json', import.meta.url), 'utf8');
  const pkg = JSON.parse(raw);
  assert.deepEqual(pkg.dependencies ?? {}, {});
  assert.deepEqual(pkg.devDependencies ?? {}, {});
  assert.equal(pkg.type, 'module');
  assert.equal(pkg.private, true);
});

test('parsed_frames_helper_rejects_non_json_writes', () => {
  assert.throws(() => parseFrames(['not json\n']));
  assert.deepEqual(parseFrames(['{"jsonrpc":"2.0","id":1,"result":{}}\n']), [
    { jsonrpc: '2.0', id: 1, result: {} },
  ]);
});

test('connection_id_constant_matches_the_contract_pattern', () => {
  assert.match(TEST_CONNECTION_ID, /^mcpconn_[0-9a-f]{16}$/u);
});
