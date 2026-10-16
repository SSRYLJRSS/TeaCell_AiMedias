import { spawn } from 'node:child_process';
import process from 'node:process';
import { Buffer } from 'node:buffer';
import { FrameDecoder, encodeFrame } from './protocol.mjs';
import { childEnvironment } from './supervisor.mjs';
import { readRequestBytes } from './request-body.mjs';

// Private stdio transport experiment. The Rust executable maps a fixed route to a mock server.
export class GatewayTransport {
  #fixtureRevision;
  #pending = new Map();
  #reading = new Set();
  #nextId = 1;
  #failure;
  #readyResolve;
  #readyReject;
  #exit;
  attempts = 0;
  chunks = 0;
  // Bounded fixture evidence: native provider SDKs may replace fetch errors
  // with "Connection error". Retain codes, never request bodies or credentials.
  errors = [];
  constructor(executable, fixturePort, { fixturePolicyPath, fixtureProxyUrl, fixtureScheme = 'http', fixtureTlsCaPath, fixtureWorkingDirectory, fixtureRevision = 1 } = {}) {
    if (fixtureProxyUrl && !/^http:\/\/127\.0\.0\.1:[1-9][0-9]{0,4}$/.test(fixtureProxyUrl)) throw new Error('INVALID_FIXTURE_PROXY');
    if (!['http', 'https'].includes(fixtureScheme) || (fixtureTlsCaPath && fixtureScheme !== 'https')) throw new Error('INVALID_FIXTURE_TLS');
    if (!Number.isSafeInteger(fixtureRevision) || fixtureRevision < 1 || fixtureRevision > 0xffffffff) throw new Error('INVALID_FIXTURE_REVISION');
    this.#fixtureRevision = fixtureRevision;
    const env = childEnvironment(process.env);
    // Test-only injection verifies Rust's explicit no_proxy against real proxy
    // listeners. Ordinary launches still use the environment allowlist alone.
    if (fixtureProxyUrl) for (const name of ['HTTP_PROXY', 'http_proxy', 'HTTPS_PROXY', 'https_proxy', 'ALL_PROXY', 'all_proxy']) env[name] = fixtureProxyUrl;
    this.ready = new Promise((resolve, reject) => { this.#readyResolve = resolve; this.#readyReject = reject; });
    this.child = spawn(executable, [String(fixturePort), fixturePolicyPath ?? '-', fixtureScheme, fixtureTlsCaPath ?? '-', String(fixtureRevision)], {
      env, cwd: fixtureWorkingDirectory, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'],
    });
    this.#exit = new Promise(resolve => this.child.once('close', (code, signal) => {
      this.#fail(new Error('GATEWAY_EXITED'));
      resolve({ code, signal });
    }));
    this.child.once('error', error => this.#fail(error));
    this.child.stdin.on('error', error => this.#fail(error));
    const decoder = new FrameDecoder();
    this.child.stdout.on('data', bytes => {
      try { for (const frame of decoder.push(bytes)) this.#receive(frame); }
      catch (error) { this.#fail(error); this.child.kill(); }
    });
    this.child.stdout.on('end', () => { try { decoder.end(); } catch (error) { this.#fail(error); } });
    let stderrSize = 0;
    this.child.stderr.on('data', bytes => { stderrSize += bytes.length; if (stderrSize > 65536) this.child.kill(); });
  }
  #send(frame) {
    if (this.#failure) throw this.#failure;
    this.child.stdin.write(encodeFrame(frame));
  }
  #finish(id, error) {
    const entry = this.#pending.get(id);
    if (!entry) return;
    this.#pending.delete(id);
    entry.signal.removeEventListener('abort', entry.abort);
    if (error) { entry.reject(error); entry.controller?.error(error); }
    else entry.controller?.close();
    entry.pulled?.();
    entry.uploadReject?.(error ?? new Error('UPLOAD_CLOSED'));
  }
  #fail(error) {
    this.#failure ??= error;
    for (const controller of this.#reading) controller.abort(error);
    this.#readyReject(error);
    for (const id of this.#pending.keys()) this.#finish(id, error);
  }
  #receive(frame) {
    if (frame.kind === 'ready' && frame.protocol === 1) { this.#readyResolve(); return; }
    if (frame.kind === 'closed') return;
    if (frame.kind === 'fatal') { this.#fail(new Error(frame.code)); this.child.kill(); return; }
    const entry = this.#pending.get(frame.id);
    if (!entry) return; // Cancellation can cross an already-written response frame.
    if (frame.kind === 'uploaded') {
      if (!entry.uploadResolve || frame.seq !== entry.uploadSeq) throw new Error('UPLOAD_SEQUENCE');
      const resolve = entry.uploadResolve;
      entry.uploadResolve = undefined;
      entry.uploadReject = undefined;
      resolve();
      return;
    }
    if (frame.kind === 'attempt') { this.attempts++; return; }
    if (frame.kind === 'head') {
      if (entry.controller) throw new Error('DUPLICATE_HEAD');
      const body = new globalThis.ReadableStream({
        start: controller => { entry.controller = controller; },
        pull: () => new Promise(resolve => { entry.pulled = resolve; this.#send({ kind: 'pull', id: frame.id }); }),
        cancel: () => entry.abort(),
      }, { highWaterMark: 0 });
      entry.resolve(new globalThis.Response(body, { status: frame.status, headers: frame.headers }));
    } else if (frame.kind === 'chunk') {
      if (!entry.controller || !entry.pulled) throw new Error('UNREQUESTED_CHUNK');
      this.chunks++;
      entry.controller.enqueue(new Uint8Array(Buffer.from(frame.data, 'base64')));
      const pulled = entry.pulled;
      entry.pulled = undefined;
      pulled();
    } else if (frame.kind === 'end') this.#finish(frame.id);
    else if (frame.kind === 'error') {
      if (typeof frame.code !== 'string' || !/^[A-Z_]{1,64}$/.test(frame.code)) throw new Error('INVALID_ERROR_CODE');
      if (this.errors.length < 64) this.errors.push({ id: frame.id, code: frame.code });
      this.#finish(frame.id, new Error(frame.code));
    }
    else throw new Error('UNKNOWN_GATEWAY_FRAME');
  }
  fetch = async (input, init) => {
    await this.ready;
    if (this.#failure) throw this.#failure;
    if (this.#pending.size + this.#reading.size >= 8) throw new Error('PENDING_LIMIT');
    const request = new globalThis.Request(input, init);
    if (request.signal.aborted) throw new globalThis.DOMException('Cancelled', 'AbortError');
    const reading = new globalThis.AbortController();
    const abortReading = () => reading.abort(new globalThis.DOMException('Cancelled', 'AbortError'));
    request.signal.addEventListener('abort', abortReading, { once: true });
    this.#reading.add(reading);
    let bytes;
    try { bytes = await readRequestBytes(request, reading.signal); }
    finally {
      this.#reading.delete(reading);
      request.signal.removeEventListener('abort', abortReading);
    }
    if (this.#failure) throw this.#failure;
    let body;
    try { body = new globalThis.TextDecoder('utf-8', { fatal: true }).decode(bytes); }
    catch { throw new Error('INVALID_BODY'); }
    if (request.signal.aborted) throw new globalThis.DOMException('Cancelled', 'AbortError');
    if (this.#pending.size >= 8) throw new Error('PENDING_LIMIT');
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      const abort = () => {
        if (this.#pending.has(id)) {
          this.#send({ kind: 'cancel', id });
          this.#finish(id, new globalThis.DOMException('Cancelled', 'AbortError'));
        }
      };
      this.#pending.set(id, { resolve, reject, signal: request.signal, abort });
      request.signal.addEventListener('abort', abort, { once: true });
      const send = async () => {
        // JSON escaping can expand a control byte to six bytes.
        if (bytes.length <= 8000) this.#send({ kind: 'start', id, method: request.method, url: request.url, body, revision: this.#fixtureRevision });
        else {
          const acknowledged = (frame, seq) => new Promise((resolve, reject) => {
            const entry = this.#pending.get(id);
            if (!entry) { reject(new globalThis.DOMException('Cancelled', 'AbortError')); return; }
            entry.uploadResolve = resolve;
            entry.uploadReject = reject;
            entry.uploadSeq = seq;
            try { this.#send(frame); } catch (error) { reject(error); }
          });
          await acknowledged({ kind: 'upload', id, method: request.method, url: request.url, length: bytes.length, revision: this.#fixtureRevision }, 0);
          let seq = 0;
          for (let offset = 0; offset < bytes.length; offset += 16384) {
            await acknowledged({ kind: 'body', id, seq, data: bytes.subarray(offset, offset + 16384).toString('base64') }, seq + 1);
            seq++;
          }
          if (this.#pending.has(id)) this.#send({ kind: 'commit', id, seq });
        }
        if (request.signal.aborted) abort();
      };
      send().catch(error => this.#finish(id, error));
    });
  };
  async close() {
    for (const controller of this.#reading) controller.abort(new Error('GATEWAY_CLOSED'));
    if (!this.#failure) this.#send({ kind: 'shutdown', id: 0 });
    this.child.stdin.end();
    return this.#exit;
  }
}
