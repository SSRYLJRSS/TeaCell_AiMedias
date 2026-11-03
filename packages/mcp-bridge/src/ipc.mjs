/**
 * Local IPC client for the TeaCell host.
 *
 * Windows x64 only: the host listens on a named pipe whose name comes from the
 * endpoint file. This module owns the only network-ish handle the bridge has, and it
 * never talks to anything but that local pipe.
 */

import net from 'node:net';
import { Buffer } from 'node:buffer';

import { FrameDecoder, PIPE_NAME_PATTERN, isPlainObject } from './protocol.mjs';

/** Stable local failure codes; the bridge maps these onto contract section 6. */
export class IpcError extends Error {
  constructor(code, detail) {
    super(detail ? `${code}: ${detail}` : code);
    this.name = 'IpcError';
    this.code = code;
  }
}

/** `\\.\pipe\<name>` on Windows. */
export function pipePath(pipeName) {
  if (typeof pipeName !== 'string' || !PIPE_NAME_PATTERN.test(pipeName)) {
    throw new IpcError('PROTOCOL_UNSUPPORTED', 'INVALID_PIPE_NAME');
  }
  return `\\\\.\\pipe\\${pipeName}`;
}

export const DEFAULT_CONNECT_TIMEOUT_MS = 5_000;

/**
 * Connect to the host pipe.
 *
 * Resolves with a transport: `{ send(frame), close(), onFrame(cb), onClose(cb) }`.
 * Rejects with `IpcError` carrying `HOST_UNAVAILABLE`, `UNSUPPORTED` or
 * `PROTOCOL_UNSUPPORTED`; callers surface those codes to the MCP client verbatim.
 */
export function connectPipe(pipeName, options = {}) {
  const { timeoutMs = DEFAULT_CONNECT_TIMEOUT_MS, socketFactory = () => new net.Socket() } = options;
  if (process.platform !== 'win32') {
    return Promise.reject(new IpcError('UNSUPPORTED', 'PLATFORM'));
  }
  let path;
  try {
    path = pipePath(pipeName);
  } catch (error) {
    return Promise.reject(error);
  }

  return new Promise((resolve, reject) => {
    let settled = false;
    const socket = socketFactory();
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try {
        socket.destroy();
      } catch {
        // Already gone.
      }
      reject(new IpcError('HOST_UNAVAILABLE', 'CONNECT_TIMEOUT'));
    }, timeoutMs);
    if (typeof timer.unref === 'function') timer.unref();

    const onConnectError = (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new IpcError('HOST_UNAVAILABLE', error?.code ?? 'CONNECT_FAILED'));
    };

    socket.once('error', onConnectError);
    socket.once('connect', () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.removeListener('error', onConnectError);
      resolve(createSocketTransport(socket));
    });
    socket.connect(path);
  });
}

function createSocketTransport(socket) {
  const frameHandlers = new Set();
  const closeHandlers = new Set();
  const decoder = new FrameDecoder();
  let closed = false;

  const emitClose = (reason) => {
    if (closed) return;
    closed = true;
    for (const handler of closeHandlers) {
      try {
        handler(reason);
      } catch {
        // A broken observer must not break the transport.
      }
    }
  };

  socket.on('data', (chunk) => {
    const { frames } = decoder.push(chunk);
    for (const frame of frames) {
      for (const handler of frameHandlers) {
        try {
          handler(frame);
        } catch {
          // Observers are isolated on purpose.
        }
      }
    }
  });
  socket.on('error', (error) => emitClose({ code: 'HOST_UNAVAILABLE', detail: error?.code ?? 'SOCKET_ERROR' }));
  socket.on('close', () => emitClose({ code: 'HOST_UNAVAILABLE', detail: 'SOCKET_CLOSED' }));

  return {
    send(frame) {
      if (closed) throw new IpcError('HOST_UNAVAILABLE', 'TRANSPORT_CLOSED');
      if (!isPlainObject(frame)) throw new IpcError('PROTOCOL_UNSUPPORTED', 'FRAME_NOT_OBJECT');
      const text = JSON.stringify(frame);
      socket.write(`${text}\n`, 'utf8');
    },
    close() {
      emitClose({ code: 'HOST_UNAVAILABLE', detail: 'CLIENT_CLOSED' });
      try {
        socket.end();
        socket.destroy();
      } catch {
        // Best effort.
      }
    },
    onFrame(handler) {
      frameHandlers.add(handler);
      return () => frameHandlers.delete(handler);
    },
    onClose(handler) {
      closeHandlers.add(handler);
      return () => closeHandlers.delete(handler);
    },
    get closed() {
      return closed;
    },
  };
}

/**
 * In-memory transport for tests and for the "host is not there" path.
 *
 * Records what the bridge sent and lets a test push frames back as if the host had
 * spoken. It never touches a real pipe.
 */
export function createMemoryTransport(_options = {}) {
  const frameHandlers = new Set();
  const closeHandlers = new Set();
  const sent = [];
  const state = { closed: false, closeReason: null };

  const transport = {
    sent,
    send(frame) {
      if (state.closed) throw new IpcError('HOST_UNAVAILABLE', 'TRANSPORT_CLOSED');
      if (!isPlainObject(frame)) throw new IpcError('PROTOCOL_UNSUPPORTED', 'FRAME_NOT_OBJECT');
      sent.push(frame);
    },
    close() {
      if (state.closed) return;
      state.closed = true;
      state.closeReason ??= { code: 'HOST_UNAVAILABLE', detail: 'CLIENT_CLOSED' };
      for (const handler of closeHandlers) handler(state.closeReason);
    },
    onFrame(handler) {
      frameHandlers.add(handler);
      return () => frameHandlers.delete(handler);
    },
    onClose(handler) {
      closeHandlers.add(handler);
      return () => closeHandlers.delete(handler);
    },
    get closed() {
      return state.closed;
    },
    get closeReason() {
      return state.closeReason;
    },
    /** Test helper: pretend the host sent a frame. */
    inject(frame) {
      for (const handler of frameHandlers) handler(frame);
    },
    /** Test helper: pretend the host dropped the connection. */
    drop(reason = { code: 'HOST_UNAVAILABLE', detail: 'HOST_CLOSED' }) {
      if (state.closed) return;
      state.closed = true;
      state.closeReason = reason;
      for (const handler of closeHandlers) handler(reason);
    },
    /** Test helper: raw bytes straight into the decoder path, for framing tests. */
    injectBytes(chunk) {
      const decoder = new FrameDecoder();
      const { frames } = decoder.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
      for (const frame of frames) transport.inject(frame);
    },
  };
  return transport;
}
