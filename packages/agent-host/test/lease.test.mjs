import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { spawn } from 'node:child_process';
import { mkdtemp, rm, mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { acquireLease } from '../src/lease.mjs';

async function exitedChildPid() {
  const child = spawn(process.execPath, ['-e', ''], { stdio: 'ignore' });
  const pid = child.pid;
  await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', resolve);
  });
  return pid;
}

test('recovery refuses a live owner and keeps its lease intact', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-lease-'));
  let release;
  try {
    const instance = randomUUID();
    release = await acquireLease(directory, instance);
    const before = await readFile(join(directory, 'host.lock', 'owner.json'), 'utf8');
    await assert.rejects(acquireLease(directory, randomUUID(), true), /LOCK_OWNER_ALIVE/);
    assert.equal(await readFile(join(directory, 'host.lock', 'owner.json'), 'utf8'), before);
  } finally {
    await release?.();
    await rm(directory, { recursive: true, force: true });
  }
});

test('malformed or unknown lock contents are preserved', async () => {
  for (const content of [null, '{"schema":1,"pid":-1,"instance":"wrong"}', 'not-json']) {
    const directory = await mkdtemp(join(tmpdir(), 'teacell-lease-'));
    try {
      const lock = join(directory, 'host.lock');
      await mkdir(lock);
      if (content !== null) await writeFile(join(lock, 'owner.json'), content);
      await assert.rejects(acquireLease(directory, randomUUID(), true));
      if (content !== null) assert.equal(await readFile(join(lock, 'owner.json'), 'utf8'), content);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
});

test('a recovery interrupted by process exit can be retried safely', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-lease-recovery-'));
  let release;
  try {
    const deadPid = await exitedChildPid();
    const staleInstance = randomUUID();
    const recoveryLock = join(directory, 'recovery.lock');
    const hostLock = join(directory, 'host.lock');
    await mkdir(recoveryLock);
    await writeFile(join(recoveryLock, 'owner.json'), JSON.stringify({ schema: 1, pid: deadPid, instance: randomUUID() }));
    await mkdir(hostLock);
    await writeFile(join(hostLock, 'owner.json'), JSON.stringify({ schema: 1, pid: deadPid, instance: staleInstance }));

    const instance = randomUUID();
    release = await acquireLease(directory, instance, true);

    assert.deepEqual(await readdir(directory), ['host.lock']);
    assert.deepEqual(JSON.parse(await readFile(join(hostLock, 'owner.json'), 'utf8')), { schema: 1, pid: process.pid, instance });
  } finally {
    await release?.();
    await rm(directory, { recursive: true, force: true });
  }
});
