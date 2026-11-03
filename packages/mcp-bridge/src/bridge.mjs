/**
 * Bridge state machine: endpoint discovery, handshake and capability calls.
 *
 * Pure logic on purpose. Every I/O concern (pipe transport, endpoint file read, clock,
 * logging) is injected, so the whole contract section 4/5/7 surface is testable
 * offline. The bridge never writes files, never opens a database and never emits
 * network traffic; it only reads the endpoint file it is told to read.
 */

import { createHmac, randomBytes } from 'node:crypto';
import { Buffer } from 'node:buffer';
import { readFile } from 'node:fs/promises';

import {
  BRIDGE_ERROR_CODE_SET,
  CONNECTION_ID_PATTERN,
  ENDPOINT_SCHEMA_VERSION,
  IPC_PROTOCOL_VERSION,
  PIPE_NAME_PATTERN,
  REQUEST_ID_PATTERN,
  decodePayload,
  encodePayload,
  isPlainObject,
} from './protocol.mjs';
import { connectPipe } from './ipc.mjs';
import { silentLogger } from './log.mjs';

/** Contract handshake budget: hello must be on the wire within five seconds. */
export const HANDSHAKE_TIMEOUT_MS = 5_000;
/** A call that never gets a result is treated as the host being gone. */
export const CALL_TIMEOUT_MS = 30_000;

/**
 * A failure that already carries one of the eight contract codes.
 *
 * `code` is the host code as the sender knows it; it goes through the same
 * `mapHostCode` translation the host replies use, so a caller that throws
 * `PLAN_CHANGED` reaches the client as `NOT_ALLOWED` and not as a fallback.
 * Falling back to `HOST_UNAVAILABLE` here would tell the user to restart a
 * healthy app when the real answer is "the plan changed, prepare it again".
 */
export class BridgeError extends Error {
  constructor(code, detail, options = {}) {
    super(detail ? `${code}: ${detail}` : code);
    this.name = 'BridgeError';
    this.code = mapHostCode(code);
    this.detail = detail ?? null;
    this.hostCode = options.hostCode ?? null;
  }
}

/**
 * Host or transport codes -> the eight codes the MCP client can act on.
 *
 * The original host code is kept on the error as `hostCode` so diagnostics stay
 * honest, but the code the client branches on is always one of the eight.
 */
const HOST_CODE_MAP = Object.freeze({
  HOST_UNAVAILABLE: 'HOST_UNAVAILABLE',
  PROTOCOL_UNSUPPORTED: 'PROTOCOL_UNSUPPORTED',
  UNSUPPORTED_PROTOCOL: 'PROTOCOL_UNSUPPORTED',
  PROTOCOL_MISMATCH: 'PROTOCOL_UNSUPPORTED',
  INVALID_FRAME: 'PROTOCOL_UNSUPPORTED',
  PAIRING_REQUIRED: 'PAIRING_REQUIRED',
  UNAUTHORIZED: 'PAIRING_REQUIRED',
  PAIRING_REVOKED: 'PAIRING_REVOKED',
  GRANT_REVOKED: 'PAIRING_REVOKED',
  NOT_ALLOWED: 'NOT_ALLOWED',
  UNSUPPORTED_CAPABILITY: 'NOT_ALLOWED',
  APPROVAL_REQUIRED: 'NOT_ALLOWED',
  PLAN_EXPIRED: 'NOT_ALLOWED',
  PLAN_CHANGED: 'NOT_ALLOWED',
  RESOURCE_CONFLICT: 'NOT_ALLOWED',
  // The host rejects a malformed payload at the frame level. Keep it distinct:
  // a client told HOST_UNAVAILABLE would go restart a healthy app.
  INVALID_INPUT: 'INVALID_INPUT',
  LIBRARY_OFFLINE: 'LIBRARY_OFFLINE',
  MAINTENANCE_ACTIVE: 'LIBRARY_OFFLINE',
  UNSUPPORTED: 'UNSUPPORTED',
  UNSUPPORTED_PLATFORM: 'UNSUPPORTED',
});

export function mapHostCode(code) {
  if (typeof code === 'string' && BRIDGE_ERROR_CODE_SET.has(code)) return code;
  if (typeof code === 'string' && HOST_CODE_MAP[code]) return HOST_CODE_MAP[code];
  return 'HOST_UNAVAILABLE';
}

function endpointError(detail) {
  return new BridgeError('PROTOCOL_UNSUPPORTED', detail);
}

/** Validates the endpoint document; anything unexpected is a protocol mismatch. */
export function parseEndpoint(document) {
  if (!isPlainObject(document)) throw endpointError('ENDPOINT_NOT_OBJECT');
  if (document.schemaVersion !== ENDPOINT_SCHEMA_VERSION) throw endpointError('ENDPOINT_SCHEMA');
  if (document.protocol !== IPC_PROTOCOL_VERSION) throw endpointError('ENDPOINT_PROTOCOL');
  if (typeof document.instance !== 'string' || document.instance.length === 0) {
    throw endpointError('ENDPOINT_INSTANCE');
  }
  if (typeof document.pipeName !== 'string' || !PIPE_NAME_PATTERN.test(document.pipeName)) {
    throw endpointError('ENDPOINT_PIPE_NAME');
  }
  if (typeof document.key !== 'string' || document.key.length === 0) {
    throw endpointError('ENDPOINT_KEY');
  }
  const keyBytes = Buffer.from(document.key, 'base64url');
  if (keyBytes.length !== 32) throw endpointError('ENDPOINT_KEY');
  return Object.freeze({
    schemaVersion: document.schemaVersion,
    protocol: document.protocol,
    instance: document.instance,
    pipeName: document.pipeName,
    key: document.key,
  });
}

/** `token = HMAC-SHA256(key, connectionId)` in lowercase hex. */
export function computeToken(key, connectionId) {
  const keyBytes = Buffer.from(key, 'base64url');
  return createHmac('sha256', keyBytes).update(connectionId, 'utf8').digest('hex');
}

/** Default endpoint reader: the only filesystem access this package is allowed. */
export async function readEndpointFile(path) {
  const text = await readFile(path, 'utf8');
  return JSON.parse(text);
}

function requestId() {
  return `req_${randomBytes(8).toString('hex')}`;
}

/**
 * The bridge.
 *
 * Lifecycle: `discover()` -> `connect()` -> `handshake()` -> `call()`... -> `close()`.
 * `open()` runs the first three and is what the server entry point uses.
 * After a `close` frame, or after the transport dies, every later `call()` throws the
 * remembered code instead of touching the transport again.
 */
export class Bridge {
  #transport;
  #endpointPath;
  #connectionId;
  #clientInfo;
  #readEndpoint;
  #connect;
  #log;
  #platform;
  #handshakeTimeoutMs;
  #callTimeoutMs;

  #endpoint = null;
  #welcome = null;
  #tools = Object.freeze([]);
  #capabilities = Object.freeze([]);
  #closeCode = null;
  #welcomeWaiter = null;
  #pending = new Map();
  #unsubscribeFrame = null;
  #unsubscribeClose = null;

  constructor(options = {}) {
    this.#transport = options.transport ?? null;
    this.#endpointPath = options.endpointPath ?? null;
    this.#connectionId = options.connectionId ?? null;
    this.#clientInfo = {
      name: options.clientInfo?.name ?? 'teacell-mcp-bridge',
      version: options.clientInfo?.version ?? '1.0.0',
    };
    this.#readEndpoint = options.readEndpoint ?? readEndpointFile;
    this.#connect = options.connect ?? connectPipe;
    this.#log = options.log ?? silentLogger;
    this.#platform = options.platform ?? process.platform;
    this.#handshakeTimeoutMs = options.handshakeTimeoutMs ?? HANDSHAKE_TIMEOUT_MS;
    this.#callTimeoutMs = options.callTimeoutMs ?? CALL_TIMEOUT_MS;
  }

  get state() {
    if (this.#closeCode) return 'closed';
    if (this.#welcome) return 'ready';
    if (this.#transport) return 'connected';
    if (this.#endpoint) return 'discovered';
    return 'idle';
  }

  get closeCode() {
    return this.#closeCode;
  }

  get endpoint() {
    return this.#endpoint;
  }

  get tools() {
    return this.#tools;
  }

  get capabilities() {
    return this.#capabilities;
  }

  get welcome() {
    return this.#welcome;
  }

  /** Reads and validates the endpoint document named on the command line. */
  async discover() {
    if (this.#platform !== 'win32') {
      throw new BridgeError('UNSUPPORTED', 'PLATFORM');
    }
    if (!this.#endpointPath) {
      throw new BridgeError('HOST_UNAVAILABLE', 'ENDPOINT_NOT_CONFIGURED');
    }
    let document;
    try {
      document = await this.#readEndpoint(this.#endpointPath);
    } catch (error) {
      if (error instanceof BridgeError) throw error;
      // Missing file, permission denied and unreadable JSON are all "the host is not
      // there" from the client's point of view: tell them to start TeaCell.
      if (error instanceof SyntaxError) throw endpointError('ENDPOINT_JSON');
      throw new BridgeError('HOST_UNAVAILABLE', error?.code ?? 'ENDPOINT_UNREADABLE');
    }
    this.#endpoint = parseEndpoint(document);
    this.#log.info('endpoint discovered', { instance: this.#endpoint.instance });
    return this.#endpoint;
  }

  /** Connects the transport (unless one was injected). */
  async connect() {
    if (this.#transport) return this.#transport;
    if (!this.#endpoint) throw new BridgeError('HOST_UNAVAILABLE', 'NOT_DISCOVERED');
    try {
      this.#transport = await this.#connect(this.#endpoint.pipeName, {
        timeoutMs: this.#handshakeTimeoutMs,
      });
    } catch (error) {
      throw new BridgeError(mapHostCode(error?.code), error?.detail ?? 'CONNECT_FAILED');
    }
    return this.#transport;
  }

  /** Sends `hello` and waits for `welcome`; the tool table comes from `welcome` alone. */
  async handshake() {
    if (this.#closeCode) throw new BridgeError(this.#closeCode, 'SESSION_CLOSED');
    const transport = this.#transport;
    if (!transport) throw new BridgeError('HOST_UNAVAILABLE', 'NOT_CONNECTED');
    if (!this.#endpoint) throw new BridgeError('HOST_UNAVAILABLE', 'NOT_DISCOVERED');
    const connectionId = this.#connectionId;
    if (typeof connectionId !== 'string' || !CONNECTION_ID_PATTERN.test(connectionId)) {
      throw new BridgeError('PAIRING_REQUIRED', 'CONNECTION_ID');
    }

    this.#attachTransport(transport);

    let token;
    try {
      token = computeToken(this.#endpoint.key, connectionId);
    } catch {
      throw new BridgeError('PAIRING_REQUIRED', 'TOKEN');
    }

    const welcome = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#welcomeWaiter = null;
        reject(new BridgeError('HOST_UNAVAILABLE', 'HANDSHAKE_TIMEOUT'));
      }, this.#handshakeTimeoutMs);
      if (typeof timer.unref === 'function') timer.unref();
      this.#welcomeWaiter = { resolve, reject, timer };
    });
    // The rejection must not become unhandled if the caller aborts early.
    welcome.catch(() => {});

    try {
      transport.send({
        type: 'hello',
        protocol: IPC_PROTOCOL_VERSION,
        connectionId,
        token,
        clientInfo: { ...this.#clientInfo },
      });
    } catch (error) {
      this.#clearWelcomeWaiter(new BridgeError(mapHostCode(error?.code), 'HELLO_SEND_FAILED'));
      throw new BridgeError(mapHostCode(error?.code), 'HELLO_SEND_FAILED');
    }

    const frame = await welcome;
    const accepted = this.#acceptWelcome(frame);
    this.#log.info('handshake accepted', {
      instance: accepted.instance,
      tools: accepted.tools.length,
    });
    return accepted;
  }

  /** discover + connect + handshake. */
  async open() {
    await this.discover();
    await this.connect();
    return this.handshake();
  }

  /**
   * Invokes one capability.
   *
   * `payload` must be a plain object; it becomes the standard-Base64 JSON payload the
   * host expects. Resolves with the decoded result object, rejects with BridgeError.
   */
  async call(capability, payload) {
    if (this.#closeCode) throw new BridgeError(this.#closeCode, 'SESSION_CLOSED');
    if (!this.#welcome) throw new BridgeError('HOST_UNAVAILABLE', 'NOT_READY');
    if (typeof capability !== 'string' || !capability) {
      throw new BridgeError('NOT_ALLOWED', 'CAPABILITY');
    }
    const tool = this.#tools.find((entry) => entry.capability === capability);
    if (!tool) throw new BridgeError('NOT_ALLOWED', 'CAPABILITY_NOT_IN_WELCOME', { hostCode: capability });
    if (!isPlainObject(payload)) {
      throw new BridgeError('INVALID_INPUT', 'PAYLOAD_NOT_OBJECT');
    }
    let encoded;
    try {
      encoded = encodePayload(payload);
    } catch (error) {
      throw new BridgeError('INVALID_INPUT', error?.code ?? 'PAYLOAD');
    }

    const id = requestId();
    if (!REQUEST_ID_PATTERN.test(id)) throw new BridgeError('HOST_UNAVAILABLE', 'REQUEST_ID');

    const result = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new BridgeError('HOST_UNAVAILABLE', 'CALL_TIMEOUT'));
      }, this.#callTimeoutMs);
      if (typeof timer.unref === 'function') timer.unref();
      this.#pending.set(id, { resolve, reject, timer });
    });
    result.catch(() => {});

    try {
      this.#transport.send({ type: 'call', requestId: id, capability, payload: encoded });
    } catch (error) {
      const entry = this.#pending.get(id);
      if (entry) {
        clearTimeout(entry.timer);
        this.#pending.delete(id);
        entry.reject(new BridgeError(mapHostCode(error?.code), 'CALL_SEND_FAILED'));
      }
    }
    return result;
  }

  /** Closes the transport and rejects anything still in flight. */
  close(reason = 'CLIENT_CLOSED') {
    this.#failPending(new BridgeError(this.#closeCode ?? 'HOST_UNAVAILABLE', reason));
    this.#clearWelcomeWaiter(new BridgeError('HOST_UNAVAILABLE', reason));
    if (this.#transport) {
      try {
        this.#transport.close();
      } catch {
        // Best effort.
      }
    }
    this.#unsubscribeFrame?.();
    this.#unsubscribeClose?.();
    this.#unsubscribeFrame = null;
    this.#unsubscribeClose = null;
    if (!this.#closeCode) this.#closeCode = 'HOST_UNAVAILABLE';
  }

  #attachTransport(transport) {
    this.#unsubscribeFrame ??= transport.onFrame((frame) => this.#onFrame(frame));
    this.#unsubscribeClose ??= transport.onClose?.((reason) => this.#onTransportClose(reason));
  }

  #onTransportClose(reason) {
    const code = mapHostCode(reason?.code);
    if (!this.#closeCode) this.#closeCode = code;
    this.#log.warn('transport closed', { code });
    this.#clearWelcomeWaiter(new BridgeError(code, 'TRANSPORT_CLOSED'));
    this.#failPending(new BridgeError(code, 'TRANSPORT_CLOSED'));
  }

  #onFrame(frame) {
    if (!isPlainObject(frame)) {
      this.#log.warn('dropping non-object frame');
      return;
    }
    switch (frame.type) {
      case 'welcome':
        this.#clearWelcomeWaiter(null, frame);
        return;
      case 'result':
        this.#onResult(frame);
        return;
      case 'close':
        this.#onCloseFrame(frame);
        return;
      case 'pong':
      case 'ping':
      default:
        // ping/pong and anything unknown on this surface are ignored; the host owns
        // liveness and the contract has no bridge-initiated request.
        return;
    }
  }

  #onResult(frame) {
    if (typeof frame.requestId !== 'string') return;
    const entry = this.#pending.get(frame.requestId);
    if (!entry) return;
    clearTimeout(entry.timer);
    this.#pending.delete(frame.requestId);
    if (frame.ok === true) {
      try {
        entry.resolve(decodePayload(frame.payload));
      } catch {
        entry.reject(new BridgeError('PROTOCOL_UNSUPPORTED', 'RESULT_PAYLOAD'));
      }
      return;
    }
    const code = mapHostCode(frame.code);
    entry.reject(
      new BridgeError(code, typeof frame.message === 'string' ? frame.message : 'CALL_FAILED', {
        hostCode: typeof frame.code === 'string' ? frame.code : null,
      }),
    );
  }

  #onCloseFrame(frame) {
    const code = mapHostCode(frame.code);
    this.#closeCode = code;
    this.#log.warn('host closed the session', { code });
    this.#clearWelcomeWaiter(new BridgeError(code, 'HOST_CLOSED'));
    this.#failPending(new BridgeError(code, 'HOST_CLOSED'));
    try {
      this.#transport?.close();
    } catch {
      // Already gone.
    }
  }

  #clearWelcomeWaiter(error, frame) {
    const waiter = this.#welcomeWaiter;
    if (!waiter) return;
    this.#welcomeWaiter = null;
    clearTimeout(waiter.timer);
    if (error) waiter.reject(error);
    else waiter.resolve(frame);
  }

  #failPending(error) {
    for (const [, entry] of this.#pending) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.#pending.clear();
  }

  /** Validates `welcome` strictly and freezes the tool table it carries. */
  #acceptWelcome(frame) {
    if (!isPlainObject(frame) || frame.type !== 'welcome') {
      throw new BridgeError('PROTOCOL_UNSUPPORTED', 'WELCOME_INVALID');
    }
    if (frame.protocol !== IPC_PROTOCOL_VERSION) {
      throw new BridgeError('PROTOCOL_UNSUPPORTED', 'WELCOME_PROTOCOL');
    }
    if (frame.instance !== this.#endpoint.instance) {
      throw new BridgeError('PROTOCOL_UNSUPPORTED', 'WELCOME_INSTANCE');
    }
    if (typeof frame.connectionId !== 'string' || frame.connectionId !== this.#connectionId) {
      throw new BridgeError('PAIRING_REQUIRED', 'WELCOME_CONNECTION');
    }
    if (!Array.isArray(frame.tools)) {
      throw new BridgeError('PROTOCOL_UNSUPPORTED', 'WELCOME_TOOLS');
    }
    const tools = frame.tools.map((tool) => {
      if (!isPlainObject(tool) || typeof tool.name !== 'string' || !tool.name) {
        throw new BridgeError('PROTOCOL_UNSUPPORTED', 'TOOL_INVALID');
      }
      if (typeof tool.capability !== 'string' || !tool.capability) {
        throw new BridgeError('PROTOCOL_UNSUPPORTED', 'TOOL_CAPABILITY');
      }
      return Object.freeze({
        name: tool.name,
        capability: tool.capability,
        access: typeof tool.access === 'string' ? tool.access : 'query',
        description: typeof tool.description === 'string' ? tool.description : '',
        inputSchema: isPlainObject(tool.inputSchema) ? tool.inputSchema : { type: 'object' },
      });
    });
    const capabilities = Array.isArray(frame.capabilities)
      ? frame.capabilities.filter((entry) => typeof entry === 'string')
      : [];

    this.#welcome = Object.freeze({
      protocol: frame.protocol,
      instance: frame.instance,
      connectionId: frame.connectionId,
      label: typeof frame.label === 'string' ? frame.label : '',
      principalId: typeof frame.principalId === 'string' ? frame.principalId : '',
    });
    this.#tools = Object.freeze(tools);
    this.#capabilities = Object.freeze(capabilities);
    return { ...this.#welcome, tools: this.#tools, capabilities: this.#capabilities };
  }
}
