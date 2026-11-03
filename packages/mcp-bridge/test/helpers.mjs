import { Buffer } from 'node:buffer';
import { randomBytes } from 'node:crypto';

import { Bridge } from '../src/bridge.mjs';
import { createMemoryTransport } from '../src/ipc.mjs';
import { createMcpServer } from '../src/server.mjs';
import { createLogger } from '../src/log.mjs';

export const TEST_CONNECTION_ID = 'mcpconn_0123456789abcdef';

export function makeEndpoint(overrides = {}) {
  return {
    schemaVersion: 1,
    protocol: 1,
    instance: 'inst_test000000000000',
    pipeName: 'teacell-mcp-v1-test',
    key: randomBytes(32).toString('base64url'),
    ...overrides,
  };
}

export function makeWelcome(endpoint, overrides = {}) {
  return {
    type: 'welcome',
    protocol: 1,
    instance: endpoint.instance,
    connectionId: TEST_CONNECTION_ID,
    label: '测试连接',
    principalId: 'principal_local',
    capabilities: ['assets.search', 'operations.get'],
    tools: [
      {
        name: 'tea_assets_search',
        capability: 'assets.search',
        access: 'query',
        description: '搜索素材',
        inputSchema: { type: 'object' },
      },
      {
        name: 'tea_operations_get',
        capability: 'operations.get',
        access: 'query',
        description: '查询操作',
        inputSchema: { type: 'object' },
      },
    ],
    ...overrides,
  };
}

/** Records logger output so tests can assert on diagnostics without touching stderr. */
export function captureLogger(level = 'debug') {
  const lines = [];
  const log = createLogger({ level, write: (line) => lines.push(line.replace(/\n$/u, '')) });
  log.lines = lines;
  return log;
}

/**
 * Builds a bridge wired to a memory transport with an injected endpoint reader.
 * The transport is seeded with whatever the fake host should answer.
 */
export function makeBridge(options = {}) {
  const endpoint = options.endpoint ?? makeEndpoint();
  const transport = options.transport ?? createMemoryTransport();
  const log = options.log ?? captureLogger();
  const bridge = new Bridge({
    transport,
    endpointPath: 'C:\\Users\\tester\\AppData\\Roaming\\TeaCell\\mcp\\endpoint.json',
    connectionId: options.connectionId ?? TEST_CONNECTION_ID,
    readEndpoint: async () => endpoint,
    platform: 'win32',
    log,
    handshakeTimeoutMs: options.handshakeTimeoutMs ?? 200,
    callTimeoutMs: options.callTimeoutMs ?? 200,
  });
  return { bridge, transport, endpoint, log };
}

/** Reads the pending hello and answers it with a welcome, as the host would. */
export async function completeHandshake(bridge, transport, endpoint, welcomeOverrides = {}) {
  const opened = bridge.handshake();
  await Promise.resolve();
  transport.inject(makeWelcome(endpoint, welcomeOverrides));
  return opened;
}

/** Builds a fully-ready bridge plus an MCP server that captures stdout lines. */
export async function makeReadyServer(options = {}) {
  const harness = makeBridge(options);
  await harness.bridge.discover();
  await completeHandshake(harness.bridge, harness.transport, harness.endpoint, options.welcome);

  const out = [];
  const server = createMcpServer({
    bridge: harness.bridge,
    write: (chunk) => out.push(chunk),
    log: harness.log,
    ready: Promise.resolve({ ok: true, error: null }),
  });
  await server.whenReady();
  return { ...harness, server, out, frames: () => parseFrames(out) };
}

/** Splits raw stdout writes into parsed JSON-RPC frames, failing on non-JSON. */
export function parseFrames(chunks) {
  const text = chunks.join('');
  const lines = text.split('\n').filter((line) => line.length > 0);
  return lines.map((line) => JSON.parse(line));
}

/** Builds a server whose host never came up. */
export async function makeUnavailableServer(code = 'HOST_UNAVAILABLE') {
  const log = captureLogger();
  const transport = createMemoryTransport();
  const bridge = new Bridge({
    transport,
    endpointPath: 'C:\\missing\\endpoint.json',
    connectionId: TEST_CONNECTION_ID,
    readEndpoint: async () => {
      const error = new Error('ENOENT');
      error.code = 'ENOENT';
      throw error;
    },
    platform: 'win32',
    log,
    handshakeTimeoutMs: 100,
    callTimeoutMs: 100,
  });
  const out = [];
  const server = createMcpServer({
    bridge,
    write: (chunk) => out.push(chunk),
    log,
    ready: Promise.resolve({ ok: false, error: { code } }),
  });
  await server.whenReady();
  return { bridge, transport, log, server, out, frames: () => parseFrames(out) };
}

export function b64(value) {
  return Buffer.from(JSON.stringify(value), 'utf8').toString('base64');
}
