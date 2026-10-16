import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { PrototypeHost } from '../supervisor.mjs';

function alive(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}

test('real SDK boots behind Rust Job barrier, streams, exits and recovers after owner kill', {
  timeout: 30000,
  skip: !process.env.TEACELL_NATIVE_OWNER && 'build native-owner and set TEACELL_NATIVE_OWNER',
}, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-native-host-'));
  const options = { nativeOwner: process.env.TEACELL_NATIVE_OWNER };
  const normal = new PrototypeHost(directory, options);
  let killed;
  let recovered;
  try {
    await normal.ready();
    normal.send({ type: 'prompt', runId: 'native-run', text: '原生宿主测试' });
    await normal.wait(h => h.frames.some(f => f.type === 'settled' && f.runId === 'native-run'));
    assert.equal(normal.frames.filter(f => f.type === 'delta').map(f => f.text).join(''), '原型流式回复');
    await normal.close();
    assert.equal(normal.exitCode, 0);
    killed = new PrototypeHost(directory, options);
    await killed.ready();
    const owner = JSON.parse(await readFile(join(directory, 'host.lock', 'owner.json'), 'utf8'));
    assert.notEqual(owner.pid, killed.child.pid);
    killed.child.kill('SIGKILL');
    await killed.wait(h => h.closed);
    const deadline = Date.now() + 5000;
    while (alive(owner.pid) && Date.now() < deadline) await delay(20);
    assert.equal(alive(owner.pid), false);
    recovered = new PrototypeHost(directory, { ...options, recover: true });
    await recovered.ready();
    assert.notEqual(recovered.instance, killed.instance);
  } finally {
    await normal.close(); await killed?.close(); await recovered?.close();
    await rm(directory, { recursive: true, force: true });
  }
});
