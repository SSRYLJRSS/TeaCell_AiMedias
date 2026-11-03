/**
 * MCP stdio server for the TeaCell bridge.
 *
 * stdout carries JSON-RPC frames and nothing else; every diagnostic goes to stderr.
 * The method surface is frozen by docs/contracts/mcp-bridge-v1.md section 2.1:
 * initialize, notifications/initialized, ping, tools/list, tools/call. There is no
 * sampling, roots, prompts or experimental Tasks support, and approval capabilities
 * (plans.approve, operations.resume, trash.empty.*) are never exposed.
 */

import process from 'node:process';
import { pathToFileURL } from 'node:url';

import {
  FrameDecoder,
  JSON_RPC_ERRORS,
  MCP_METHODS,
  MCP_PROTOCOL_VERSION,
  SERVER_NAME,
  SERVER_VERSION,
  isPlainObject,
} from './protocol.mjs';
import { Bridge, BridgeError, mapHostCode } from './bridge.mjs';
import { createLogger } from './log.mjs';

/** Reads `--name value` / `--name=value` from argv without any dependency. */
export function parseArgs(argv, env = {}) {
  const options = {
    endpointPath: env.TEACELL_MCP_ENDPOINT ?? null,
    connectionId: env.TEACELL_MCP_CONNECTION ?? null,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (typeof arg !== 'string') continue;
    const match = /^--([a-zA-Z][\w-]*)(?:=(.*))?$/u.exec(arg);
    if (!match) continue;
    const [, name, inline] = match;
    const value = inline !== undefined ? inline : argv[index + 1];
    if (inline === undefined) index += 1;
    if (name === 'endpoint') options.endpointPath = value;
    if (name === 'connection') options.connectionId = value;
  }
  return options;
}

function textResult(value) {
  return {
    content: [{ type: 'text', text: typeof value === 'string' ? value : JSON.stringify(value) }],
  };
}

function errorResult(code, message) {
  return {
    content: [{ type: 'text', text: `${code}: ${message}` }],
    isError: true,
  };
}

/**
 * Builds the JSON-RPC engine around an injected bridge.
 *
 * `write(frame)` receives already-serialised JSON-RPC frames and `log` receives
 * diagnostics, so the entire MCP surface is drivable offline. `ready` is a promise
 * resolving to `{ ok: boolean, error?: { code } }` describing host availability.
 */
export function createMcpServer(options) {
  const bridge = options.bridge;
  const write = options.write;
  const log = options.log;
  const ready = options.ready ?? Promise.resolve({ ok: true });
  let initialized = false;
  let protocolSeen = null;
  let readiness = { ok: bridge.state === 'ready', error: null };

  ready.then((value) => {
    readiness = value && typeof value === 'object' ? value : { ok: Boolean(value), error: null };
  }, () => {
    readiness = { ok: false, error: { code: 'HOST_UNAVAILABLE' } };
  });

  const sendFrame = (frame) => write(`${JSON.stringify(frame)}\n`);
  const respond = (id, result) => sendFrame({ jsonrpc: '2.0', id: id ?? null, result });
  const respondError = (id, code, message) => {
    sendFrame({ jsonrpc: '2.0', id: id ?? null, error: { code, message } });
  };

  async function settle() {
    try {
      readiness = await ready;
    } catch {
      readiness = { ok: false, error: { code: 'HOST_UNAVAILABLE' } };
    }
    return readiness;
  }

  function unavailableCode() {
    return mapHostCode(readiness?.error?.code ?? bridge.closeCode ?? 'HOST_UNAVAILABLE');
  }

  async function toolsList(id) {
    await settle();
    if (!readiness.ok || bridge.state !== 'ready') {
      log.error('tools/list served an empty table; host is not available', {
        code: unavailableCode(),
        initialized,
      });
      respond(id, { tools: [] });
      return;
    }
    respond(id, { tools: bridge.tools });
  }

  async function toolsCall(id, params) {
    const name = params?.name;
    if (typeof name !== 'string' || !name) {
      respondError(id, JSON_RPC_ERRORS.INVALID_PARAMS, 'tools/call requires params.name');
      return;
    }
    const args = params?.arguments === undefined ? {} : params.arguments;
    if (!isPlainObject(args)) {
      respondError(id, JSON_RPC_ERRORS.INVALID_PARAMS, 'tools/call params.arguments must be an object');
      return;
    }

    await settle();
    if (!readiness.ok) {
      respond(id, errorResult(unavailableCode(), 'TeaCell host is not reachable; start TeaCell first'));
      return;
    }
    if (bridge.state !== 'ready') {
      respond(id, errorResult(unavailableCode(), 'bridge session is closed; restart the MCP client'));
      return;
    }

    const tool = bridge.tools.find((entry) => entry.name === name);
    if (!tool) {
      respondError(id, JSON_RPC_ERRORS.INVALID_PARAMS, `unknown tool: ${name}`);
      return;
    }

    try {
      const result = await bridge.call(tool.capability, args);
      respond(id, textResult(result));
    } catch (error) {
      const code = mapHostCode(error?.code);
      const detail = typeof error?.detail === 'string' && error.detail ? error.detail : 'call failed';
      log.warn('tools/call failed', { tool: name, code });
      respond(id, errorResult(code, detail));
    }
  }

  async function handle(request) {
    const isNotification = request.id === undefined;
    const id = isNotification ? undefined : request.id;
    const method = request.method;

    switch (method) {
      case MCP_METHODS.INITIALIZE: {
        protocolSeen = typeof request.params?.protocolVersion === 'string'
          ? request.params.protocolVersion
          : null;
        if (isNotification) return;
        // Version negotiation is deliberately one-way: we always answer with the
        // frozen version and the client downgrades.
        respond(id, {
          protocolVersion: MCP_PROTOCOL_VERSION,
          capabilities: { tools: {} },
          serverInfo: { name: SERVER_NAME, version: SERVER_VERSION },
        });
        return;
      }
      case MCP_METHODS.INITIALIZED:
        initialized = true;
        return;
      case MCP_METHODS.PING:
        if (!isNotification) respond(id, {});
        return;
      case MCP_METHODS.TOOLS_LIST:
        if (!isNotification) await toolsList(id);
        return;
      case MCP_METHODS.TOOLS_CALL:
        if (isNotification) return;
        await toolsCall(id, request.params);
        return;
      default:
        if (!isNotification) {
          respondError(id, JSON_RPC_ERRORS.METHOD_NOT_FOUND, `method not supported: ${String(method)}`);
        }
        return;
    }
  }

  /** Handles one already-parsed JSON value. Never throws. */
  async function handleMessage(message) {
    if (!isPlainObject(message)) {
      respondError(null, JSON_RPC_ERRORS.INVALID_REQUEST, 'request must be a JSON object');
      return;
    }
    if (message.jsonrpc !== '2.0' || typeof message.method !== 'string') {
      respondError(message.id ?? null, JSON_RPC_ERRORS.INVALID_REQUEST, 'malformed JSON-RPC request');
      return;
    }
    try {
      await handle(message);
    } catch (error) {
      log.error('internal error while handling a request', { name: error?.name });
      respondError(message.id ?? null, JSON_RPC_ERRORS.INTERNAL_ERROR, 'internal error');
    }
  }

  /** Handles one raw line (without the trailing LF). Never throws. */
  async function handleLine(line) {
    const text = typeof line === 'string' ? line : String(line ?? '');
    if (!text.trim()) {
      log.warn('ignoring empty input line');
      return;
    }
    let message;
    try {
      message = JSON.parse(text);
    } catch {
      log.warn('ignoring malformed JSON input');
      respondError(null, JSON_RPC_ERRORS.PARSE_ERROR, 'parse error');
      return;
    }
    await handleMessage(message);
  }

  /**
   * Reports a stream-level framing problem.
   *
   * Recoverable cases answer with the JSON-RPC error the spec asks for; truncation and
   * oversize are unframeable, so they are logged and the stream keeps going.
   */
  function handleFramingError(error) {
    const code = error?.code;
    if (code === 'EMPTY_FRAME') return;
    if (code === 'INVALID_JSON' || code === 'INVALID_UTF8') {
      log.warn('stdin frame is not valid JSON', { code });
      respondError(null, JSON_RPC_ERRORS.PARSE_ERROR, 'parse error');
      return;
    }
    if (code === 'NOT_AN_OBJECT') {
      log.warn('stdin frame is not a JSON object', { code });
      respondError(null, JSON_RPC_ERRORS.INVALID_REQUEST, 'request must be a JSON object');
      return;
    }
    log.warn('discarding unframeable stdin input', { code });
  }

  return {
    handleLine,
    handleMessage,
    handleFramingError,
    get initialized() {
      return initialized;
    },
    get lastProtocolVersion() {
      return protocolSeen;
    },
    async whenReady() {
      return settle();
    },
  };
}

/** Builds the bridge with the pieces the MCP server needs, all injectable. */
export function createBridgeFromOptions(options, log = createLogger()) {
  return new Bridge({
    endpointPath: options.endpointPath,
    connectionId: options.connectionId,
    log,
    clientInfo: { name: SERVER_NAME, version: SERVER_VERSION },
  });
}

/**
 * Process assembly: argv -> endpoint -> pipe -> handshake -> stdio loop.
 *
 * A missing or unreachable host never prevents the MCP session from starting; the tool
 * table is simply empty and every call fails with HOST_UNAVAILABLE.
 */
export async function run(options = {}) {
  const argv = options.argv ?? process.argv.slice(2);
  const env = options.env ?? process.env;
  const stdin = options.stdin ?? process.stdin;
  const stdout = options.stdout ?? process.stdout;
  const log = options.log ?? createLogger();
  const parsed = options.parsed ?? parseArgs(argv, env);

  if (!parsed.endpointPath) {
    log.error('no endpoint file configured; pass --endpoint or TEACELL_MCP_ENDPOINT');
  }
  if (!parsed.connectionId) {
    log.error('no connection id configured; pass --connection or TEACELL_MCP_CONNECTION');
  }

  const bridge = options.bridge ?? createBridgeFromOptions(parsed, log);

  const ready = (async () => {
    try {
      await bridge.discover();
      await bridge.connect();
      await bridge.handshake();
      return { ok: true, error: null };
    } catch (error) {
      const code = mapHostCode(error?.code);
      log.error('host is not available; starting with an empty tool table', { code });
      return { ok: false, error: { code } };
    }
  })();

  const server = createMcpServer({
    bridge,
    write: (chunk) => stdout.write(chunk),
    log,
    ready,
  });

  const decoder = new FrameDecoder();
  stdin.on('data', (chunk) => {
    const { frames, errors } = decoder.push(chunk);
    for (const error of errors) server.handleFramingError(error);
    for (const frame of frames) void server.handleMessage(frame);
  });
  stdin.on('end', () => {
    const { errors } = decoder.end();
    for (const error of errors) log.warn('truncated stdin frame', { code: error.code });
    bridge.close('STDIN_END');
  });
  stdin.on('error', () => bridge.close('STDIN_ERROR'));

  return { server, bridge, ready };
}

const invokedDirectly = (() => {
  try {
    const entry = process.argv[1];
    if (!entry) return false;
    return import.meta.url === pathToFileURL(entry).href;
  } catch {
    return false;
  }
})();

if (invokedDirectly) {
  run().catch((error) => {
    process.stderr.write(`[mcp-bridge] fatal ${error?.name ?? 'error'}\n`);
    process.exitCode = 1;
  });
}

export { BridgeError };
