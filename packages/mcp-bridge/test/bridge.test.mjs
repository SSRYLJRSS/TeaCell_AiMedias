import test from 'node:test';
import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';

import { Bridge, BridgeError, computeToken, mapHostCode, parseEndpoint } from '../src/bridge.mjs';
import { createMemoryTransport, pipePath } from '../src/ipc.mjs';
import {
  TEST_CONNECTION_ID,
  b64,
  captureLogger,
  completeHandshake,
  makeBridge,
  makeEndpoint,
  makeWelcome,
} from './helpers.mjs';

test('token_is_hmac_sha256_of_connection_id_in_lowercase_hex', () => {
  const endpoint = makeEndpoint({ key: Buffer.alloc(32, 7).toString('base64url') });
  const token = computeToken(endpoint.key, TEST_CONNECTION_ID);
  assert.match(token, /^[0-9a-f]{64}$/u);
  assert.equal(token, computeToken(endpoint.key, TEST_CONNECTION_ID));
  assert.notEqual(token, computeToken(endpoint.key, 'mcpconn_ffffffffffffffff'));
});

test('discover_reads_the_injected_endpoint_document', async () => {
  const bridge = new Bridge({
    endpointPath: 'C:\\x\\endpoint.json',
    connectionId: TEST_CONNECTION_ID,
    readEndpoint: async () => makeEndpoint(),
    platform: 'win32',
  });
  const endpoint = await bridge.discover();
  assert.equal(endpoint.instance, 'inst_test000000000000');
  assert.equal(bridge.state, 'discovered');
  assert.equal(bridge.closeCode, null);
  assert.equal(bridge.tools.length, 0);
});

test('discover_maps_a_missing_endpoint_file_to_host_unavailable', async () => {
  const bridge = new Bridge({
    transport: createMemoryTransport(),
    endpointPath: 'C:\\nope\\endpoint.json',
    connectionId: TEST_CONNECTION_ID,
    readEndpoint: async () => {
      const error = new Error('ENOENT');
      error.code = 'ENOENT';
      throw error;
    },
    platform: 'win32',
  });
  await assert.rejects(bridge.discover(), (error) => {
    assert.ok(error instanceof BridgeError);
    assert.equal(error.code, 'HOST_UNAVAILABLE');
    return true;
  });
});

test('discover_rejects_a_wrong_protocol_as_protocol_unsupported', async () => {
  const bridge = new Bridge({
    endpointPath: 'C:\\x\\endpoint.json',
    connectionId: TEST_CONNECTION_ID,
    readEndpoint: async () => makeEndpoint({ protocol: 2 }),
    platform: 'win32',
  });
  await assert.rejects(bridge.discover(), (error) => error.code === 'PROTOCOL_UNSUPPORTED');
});

test('non_windows_platforms_are_unsupported', async () => {
  const bridge = new Bridge({
    endpointPath: 'C:\\x\\endpoint.json',
    connectionId: TEST_CONNECTION_ID,
    readEndpoint: async () => makeEndpoint(),
    platform: 'linux',
  });
  await assert.rejects(bridge.discover(), (error) => error.code === 'UNSUPPORTED');
});

test('handshake_sends_hello_with_the_contract_fields', async () => {
  const harness = makeBridge();
  await harness.bridge.discover();
  await completeHandshake(harness.bridge, harness.transport, harness.endpoint);

  assert.equal(harness.transport.sent.length, 1);
  const hello = harness.transport.sent[0];
  assert.deepEqual(Object.keys(hello).sort(), ['clientInfo', 'connectionId', 'protocol', 'token', 'type']);
  assert.equal(hello.type, 'hello');
  assert.equal(hello.protocol, 1);
  assert.equal(hello.connectionId, TEST_CONNECTION_ID);
  assert.equal(hello.token, computeToken(harness.endpoint.key, TEST_CONNECTION_ID));
  assert.deepEqual(hello.clientInfo, { name: 'teacell-mcp-bridge', version: '1.0.0' });
  assert.equal(harness.bridge.state, 'ready');
});

test('welcome_is_the_only_source_of_the_tool_table', async () => {
  const harness = makeBridge();
  await harness.bridge.discover();
  const welcome = makeWelcome(harness.endpoint, {
    tools: [
      { name: 'tea_assets_search', capability: 'assets.search', description: 'x', inputSchema: {} },
    ],
  });
  const opened = harness.bridge.handshake();
  harness.transport.inject(welcome);
  const accepted = await opened;

  assert.equal(accepted.tools.length, 1);
  assert.equal(harness.bridge.tools.length, 1);
  assert.equal(harness.bridge.tools[0].name, 'tea_assets_search');
  assert.deepEqual(harness.bridge.capabilities, ['assets.search', 'operations.get']);
  // Nothing is invented on top of the welcome frame.
  assert.equal(harness.bridge.tools.some((tool) => tool.name === 'tea_assets_trash_submit'), false);
});

test('invalid_connection_id_fails_before_any_frame_is_sent', async () => {
  const harness = makeBridge({ connectionId: 'not-a-connection' });
  await harness.bridge.discover();
  await assert.rejects(harness.bridge.handshake(), (error) => error.code === 'PAIRING_REQUIRED');
  assert.equal(harness.transport.sent.length, 0);
});

test('already_revoked_connection_surfaces_pairing_revoked', async () => {
  const harness = makeBridge();
  await harness.bridge.discover();
  const opened = harness.bridge.handshake();
  harness.transport.inject({ type: 'close', code: 'PAIRING_REVOKED', message: '连接已撤销' });
  await assert.rejects(opened, (error) => {
    assert.equal(error.code, 'PAIRING_REVOKED');
    return true;
  });
  assert.equal(harness.bridge.closeCode, 'PAIRING_REVOKED');
});

test('handshake_timeout_is_host_unavailable', async () => {
  const harness = makeBridge({ handshakeTimeoutMs: 20 });
  await harness.bridge.discover();
  await assert.rejects(harness.bridge.handshake(), (error) => error.code === 'HOST_UNAVAILABLE');
});

test('call_sends_a_base64_payload_and_decodes_the_result', async () => {
  const harness = makeBridge();
  await harness.bridge.discover();
  await completeHandshake(harness.bridge, harness.transport, harness.endpoint);

  const pending = harness.bridge.call('assets.search', { query: '猫', limit: 3 });
  await Promise.resolve();
  const call = harness.transport.sent[1];
  assert.equal(call.type, 'call');
  assert.match(call.requestId, /^req_[0-9a-f]{16}$/u);
  assert.equal(call.capability, 'assets.search');
  assert.deepEqual(JSON.parse(Buffer.from(call.payload, 'base64').toString('utf8')), {
    query: '猫',
    limit: 3,
  });

  harness.transport.inject({
    type: 'result',
    requestId: call.requestId,
    ok: true,
    payload: b64({ items: [{ assetId: 'asset_1' }] }),
  });
  assert.deepEqual(await pending, { items: [{ assetId: 'asset_1' }] });
});

test('call_rejects_capabilities_outside_the_welcome_scope', async () => {
  const harness = makeBridge();
  await harness.bridge.discover();
  await completeHandshake(harness.bridge, harness.transport, harness.endpoint);
  await assert.rejects(
    harness.bridge.call('plans.approve', {}),
    (error) => error.code === 'NOT_ALLOWED',
  );
  assert.equal(harness.transport.sent.length, 1);
});

test('call_rejects_non_object_payloads', async () => {
  const harness = makeBridge();
  await harness.bridge.discover();
  await completeHandshake(harness.bridge, harness.transport, harness.endpoint);
  // 载荷写错是调用方自己的问题，必须报 INVALID_INPUT 让人去改请求；
  // 报 NOT_ALLOWED 会被读成「没有权限」，报 HOST_UNAVAILABLE 会让人去重启一个健康的宿主。
  await assert.rejects(harness.bridge.call('assets.search', []), (error) => error.code === 'INVALID_INPUT');
});

test('bridge_error_maps_its_own_code_instead_of_collapsing_to_host_unavailable', () => {
  // 这一组覆盖「桥接自己在构造函数/入口处拒绝」的方向。宿主回包方向的映射由下一条用例覆盖；
  // 两个方向曾经不一致：BridgeError 只认八个码字面量，其余一律折叠成 HOST_UNAVAILABLE，
  // 于是外部客户端把「计划内容已变化，请重新准备」读成「宿主没启动，去重启应用」。
  const cases = [
    ['PLAN_CHANGED', 'NOT_ALLOWED'],
    ['PLAN_EXPIRED', 'NOT_ALLOWED'],
    ['APPROVAL_REQUIRED', 'NOT_ALLOWED'],
    ['RESOURCE_CONFLICT', 'NOT_ALLOWED'],
    ['UNSUPPORTED_CAPABILITY', 'NOT_ALLOWED'],
    ['PAIRING_REQUIRED', 'PAIRING_REQUIRED'],
    ['GRANT_REVOKED', 'PAIRING_REVOKED'],
    ['MAINTENANCE_ACTIVE', 'LIBRARY_OFFLINE'],
    ['INVALID_INPUT', 'INVALID_INPUT'],
    ['UNSUPPORTED_PLATFORM', 'UNSUPPORTED'],
    ['HOST_UNAVAILABLE', 'HOST_UNAVAILABLE'],
    ['SOMETHING_NEW', 'HOST_UNAVAILABLE'],
  ];
  for (const [code, expected] of cases) {
    const error = new BridgeError(code, '脱敏消息');
    assert.equal(error.code, expected, code);
    assert.equal(error.message, `${code}: 脱敏消息`);
  }
});

test('failed_result_maps_host_codes_onto_the_eight_codes', async () => {
  const harness = makeBridge();
  await harness.bridge.discover();
  await completeHandshake(harness.bridge, harness.transport, harness.endpoint);

  const cases = [
    ['PAIRING_REVOKED', 'PAIRING_REVOKED'],
    ['GRANT_REVOKED', 'PAIRING_REVOKED'],
    ['UNSUPPORTED_PROTOCOL', 'PROTOCOL_UNSUPPORTED'],
    ['NOT_ALLOWED', 'NOT_ALLOWED'],
    ['LIBRARY_OFFLINE', 'LIBRARY_OFFLINE'],
    ['UNSUPPORTED_PLATFORM', 'UNSUPPORTED'],
    // A malformed payload is the client's own fault; folding it into
    // HOST_UNAVAILABLE would tell the user to restart a healthy app.
    ['INVALID_INPUT', 'INVALID_INPUT'],
    ['SOMETHING_NEW', 'HOST_UNAVAILABLE'],
  ];

  for (const [hostCode, expected] of cases) {
    const pending = harness.bridge.call('assets.search', { query: 'x' });
    await Promise.resolve();
    const call = harness.transport.sent[harness.transport.sent.length - 1];
    harness.transport.inject({
      type: 'result',
      requestId: call.requestId,
      ok: false,
      code: hostCode,
      message: '脱敏中文消息',
    });
    await assert.rejects(pending, (error) => {
      assert.equal(error.code, expected, hostCode);
      assert.equal(error.detail, '脱敏中文消息');
      return true;
    });
  }
  assert.equal(mapHostCode('APPROVAL_REQUIRED'), 'NOT_ALLOWED');
});

test('close_frame_stops_later_calls', async () => {
  const harness = makeBridge();
  await harness.bridge.discover();
  await completeHandshake(harness.bridge, harness.transport, harness.endpoint);

  harness.transport.inject({ type: 'close', code: 'LIBRARY_OFFLINE' });
  assert.equal(harness.bridge.closeCode, 'LIBRARY_OFFLINE');

  const sentBefore = harness.transport.sent.length;
  await assert.rejects(harness.bridge.call('assets.search', {}), (error) => error.code === 'LIBRARY_OFFLINE');
  assert.equal(harness.transport.sent.length, sentBefore);
});

test('in_flight_calls_fail_closed_when_the_host_disappears', async () => {
  const harness = makeBridge();
  await harness.bridge.discover();
  await completeHandshake(harness.bridge, harness.transport, harness.endpoint);

  const pending = harness.bridge.call('assets.search', { query: 'x' });
  await Promise.resolve();
  harness.transport.drop({ code: 'HOST_UNAVAILABLE' });
  await assert.rejects(pending, (error) => error.code === 'HOST_UNAVAILABLE');
});

test('endpoint_documents_are_validated_field_by_field', () => {
  assert.throws(() => parseEndpoint(null), /PROTOCOL_UNSUPPORTED/u);
  assert.throws(() => parseEndpoint({ ...makeEndpoint(), schemaVersion: 2 }), /PROTOCOL_UNSUPPORTED/u);
  assert.throws(() => parseEndpoint({ ...makeEndpoint(), pipeName: 'bad name!' }), /PROTOCOL_UNSUPPORTED/u);
  assert.throws(() => parseEndpoint({ ...makeEndpoint(), key: 'short' }), /PROTOCOL_UNSUPPORTED/u);
  assert.equal(parseEndpoint(makeEndpoint()).protocol, 1);
});

test('pipe_path_is_the_windows_named_pipe_form', () => {
  assert.equal(pipePath('teacell-mcp-v1-abc'), '\\\\.\\pipe\\teacell-mcp-v1-abc');
  assert.throws(() => pipePath('bad/name'), /PROTOCOL_UNSUPPORTED/u);
});

test('logs_never_contain_the_key_token_or_paths', async () => {
  const log = captureLogger();
  const harness = makeBridge({ log });
  await harness.bridge.discover();
  await completeHandshake(harness.bridge, harness.transport, harness.endpoint, {
    connectionId: TEST_CONNECTION_ID,
  });
  harness.transport.inject({ type: 'close', code: 'PAIRING_REVOKED' });

  const token = computeToken(harness.endpoint.key, TEST_CONNECTION_ID);
  const output = log.lines.join('\n');
  assert.ok(!output.includes(harness.endpoint.key), 'key leaked');
  assert.ok(!output.includes(token), 'token leaked');
  assert.ok(!output.includes(TEST_CONNECTION_ID), 'connection id leaked');
  assert.ok(!output.includes('AppData'), 'path leaked');
  assert.ok(output.includes('PAIRING_REVOKED'));
});
