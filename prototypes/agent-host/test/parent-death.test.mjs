import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { childEnvironment, PrototypeHost } from '../supervisor.mjs';

function running(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}

test('strong termination of test parent closes pipe and reaps its owned SDK host', { timeout: 20000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-parent-'));
  const parent = spawn(process.execPath, [fileURLToPath(new URL('./fixtures/parent.mjs', import.meta.url)), directory], {
    cwd: directory, env: childEnvironment(process.env), windowsHide: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let childPid;
  let recovered;
  try {
    const owner = await new Promise((resolve, reject) => {
      let data = '';
      parent.stdout.on('data', bytes => {
        data += bytes.toString();
        if (data.length > 4096) reject(new Error('OWNER_OUTPUT_LIMIT'));
        if (data.includes('\n')) {
          try { resolve(JSON.parse(data.split('\n')[0])); } catch (error) { reject(error); }
        }
      });
      parent.on('error', reject);
      parent.on('exit', () => reject(new Error('OWNER_EXITED_BEFORE_HANDSHAKE')));
    });
    childPid = owner.pid;
    assert.ok(Number.isSafeInteger(childPid) && childPid > 0 && childPid !== parent.pid);
    assert.ok(running(childPid));
    const exited = new Promise(resolve => parent.once('exit', resolve));
    parent.kill('SIGKILL');
    await exited;
    const deadline = Date.now() + 5000;
    while (running(childPid) && Date.now() < deadline) await delay(20);
    assert.equal(running(childPid), false);
    // Hard termination may leave the lease record: recover explicitly, never kill
    // arbitrary matching processes or infer business completion from the lease.
    recovered = new PrototypeHost(directory, { recover: true });
    await recovered.ready();
    assert.notEqual(recovered.instance, owner.instance);
  } finally {
    parent.kill('SIGKILL');
    if (childPid && running(childPid)) process.kill(childPid, 'SIGKILL');
    await recovered?.close();
    await rm(directory, { recursive: true, force: true });
  }
});
