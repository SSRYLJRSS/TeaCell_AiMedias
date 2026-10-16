import { spawn } from 'node:child_process';
import process from 'node:process';
import { randomUUID } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout, clearTimeout } from 'node:timers';
import { FrameDecoder, encodeFrame, PROTOCOL_VERSION, SDK_VERSION } from './protocol.mjs';

export function childEnvironment(parent) {
  const allowed = new Set(['systemroot', 'windir', 'temp', 'tmp', 'tmpdir', 'lang', 'lc_all']);
  // Windows Node spawning can supply the parent's PATH when the key is absent.
  // An explicit empty value prevents ambient executable search; all owned
  // runtimes are launched by absolute path.
  return { ...Object.fromEntries(Object.entries(parent).filter(([name]) => allowed.has(name.toLowerCase()))), PATH: '' };
}

export class PrototypeHost {
  frames = [];
  events = new EventEmitter();
  instance = randomUUID();
  failure;
  closed = false;
  #exitPromise;
  #resolveExit;
  constructor(directory, { executable = process.env.TEACELL_POC_NODE ?? process.execPath,
    recover = false, nativeOwner, entrypoint, faultScenario } = {}) {
    const bootstrap = entrypoint ?? fileURLToPath(new URL('./bootstrap.mjs', import.meta.url));
    const args = nativeOwner ? [executable, bootstrap, directory, directory, this.instance]
      : [bootstrap, directory, this.instance];
    if (recover) args.push('--recover-stranded');
    if (faultScenario) args.push(faultScenario);
    this.#exitPromise = new Promise(resolve => { this.#resolveExit = resolve; });
    this.child = spawn(nativeOwner ?? executable, args, {
      cwd: directory, env: childEnvironment(process.env), windowsHide: true,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    if (!nativeOwner) this.child.once('spawn', () => this.child.stdin.write('start\n'));
    const decoder = new FrameDecoder();
    let lastSequence = 0;
    this.child.stdout.on('data', bytes => {
      try {
        for (const frame of decoder.push(bytes)) {
          if (frame.protocol !== PROTOCOL_VERSION || frame.instance !== this.instance ||
              !Number.isSafeInteger(frame.sequence) || frame.sequence <= lastSequence) {
            throw new Error('INVALID_EVENT');
          }
          lastSequence = frame.sequence;
          if (this.frames.length >= 512) throw new Error('EVENT_LIMIT');
          this.frames.push(frame);
          this.events.emit('changed');
        }
      } catch (error) { this.#fail(error); }
    });
    // Drain stderr without storing payloads that might contain untrusted input.
    let stderrBytes = 0;
    this.child.stderr.on('data', bytes => {
      stderrBytes += bytes.length;
      if (stderrBytes > 64 * 1024) this.#fail(new Error('STDERR_LIMIT'));
    });
    this.child.stdin.on('error', error => this.#fail(error));
    this.child.on('error', error => this.#fail(error));
    this.child.on('close', code => {
      this.closed = true;
      this.exitCode = code;
      this.#resolveExit();
      try { decoder.end(); } catch (error) { this.failure ??= error; }
      this.events.emit('changed');
    });
  }
  #fail(error) {
    this.failure ??= error;
    this.child.kill();
    this.events.emit('changed');
  }
  wait(predicate, timeout = 10000) {
    return new Promise((resolve, reject) => {
      const check = () => {
        if (this.failure) finish(this.failure);
        else if (predicate(this)) finish();
        else if (this.closed) finish(new Error('HOST_EXITED'));
      };
      const finish = error => {
        clearTimeout(timer);
        this.events.off('changed', check);
        if (error) reject(error); else resolve(this);
      };
      const timer = setTimeout(() => {
        this.#fail(new Error('HOST_TIMEOUT'));
      }, timeout);
      this.events.on('changed', check);
      check();
    });
  }
  async ready() {
    await this.wait(host => host.frames.some(frame => frame.type === 'ready'));
    if (this.failure) throw this.failure;
    const frame = this.frames.find(frame => frame.type === 'ready');
    if (frame.sdk !== SDK_VERSION || frame.tools.length !== 0) {
      this.#fail(new Error('HANDSHAKE_MISMATCH'));
      throw this.failure;
    }
  }
  send(payload) {
    if (this.closed || this.failure) throw new Error('HOST_UNAVAILABLE');
    if (!this.child.stdin.write(encodeFrame({ protocol: PROTOCOL_VERSION, instance: this.instance, ...payload }))) {
      this.#fail(new Error('INPUT_BACKPRESSURE'));
      throw this.failure;
    }
  }
  async close() {
    if (this.closed) return;
    if (!this.failure) {
      try { this.send({ type: 'shutdown' }); } catch { this.child.kill(); }
    }
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.child.kill();
        reject(new Error('HOST_REAP_TIMEOUT'));
      }, 5000);
      this.#exitPromise.then(() => { clearTimeout(timeout); resolve(); });
    });
  }
}
