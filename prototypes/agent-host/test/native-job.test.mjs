import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { tmpdir } from 'node:os';
import { childEnvironment } from '../supervisor.mjs';

function alive(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}

test('Windows Job reaps owned descendants on owner kill and preserves unrelated Node', {
  timeout: 20000,
  skip: !process.env.TEACELL_NATIVE_OWNER && 'build native-owner and explicitly set TEACELL_NATIVE_OWNER',
}, async () => {
  const unrelated = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {
    windowsHide: true, stdio: 'ignore',
  });
  await once(unrelated, 'spawn');
  const owner = spawn(process.env.TEACELL_NATIVE_OWNER, [process.execPath,
    fileURLToPath(new URL('./fixtures/native-tree.mjs', import.meta.url)), tmpdir()], {
    env: childEnvironment(process.env), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let pids;
  try {
    pids = await new Promise((resolve, reject) => {
      let buffer = '';
      owner.stdout.on('data', bytes => {
        buffer += bytes.toString();
        if (buffer.length > 4096) reject(new Error('OWNER_OUTPUT_LIMIT'));
        if (buffer.includes('\n')) {
          try { resolve(JSON.parse(buffer.split('\n')[0])); } catch (error) { reject(error); }
        }
      });
      owner.on('error', reject);
      owner.on('exit', () => reject(new Error('OWNER_EXITED_BEFORE_READY')));
    });
    assert.ok(Number.isSafeInteger(pids.child) && Number.isSafeInteger(pids.descendant));
    assert.ok(alive(pids.child) && alive(pids.descendant));
    const exit = once(owner, 'exit');
    owner.kill('SIGKILL');
    await exit;
    const deadline = Date.now() + 5000;
    while ((alive(pids.child) || alive(pids.descendant)) && Date.now() < deadline) await delay(20);
    assert.equal(alive(pids.child), false);
    assert.equal(alive(pids.descendant), false);
    assert.ok(alive(unrelated.pid));
  } finally {
    owner.kill('SIGKILL');
    for (const pid of Object.values(pids ?? {})) if (alive(pid)) process.kill(pid, 'SIGKILL');
    const exit = once(unrelated, 'exit');
    unrelated.kill('SIGKILL');
    await exit;
  }
});
