import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, mkdir, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { acquireLease } from '../lease.mjs';

test('explicit recovery refuses live owner and preserves its lease', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-lease-'));
  let release;
  try {
    const instance = randomUUID();
    release = await acquireLease(directory, instance);
    const before = await readFile(join(directory, 'host.lock', 'owner.json'), 'utf8');
    await assert.rejects(acquireLease(directory, randomUUID(), true), /LOCK_OWNER_ALIVE/);
    assert.equal(await readFile(join(directory, 'host.lock', 'owner.json'), 'utf8'), before);
  } finally { await release?.(); await rm(directory, { recursive: true, force: true }); }
});

test('unknown and malformed owner records are retained rather than recovered', async () => {
  for (const content of [null, '{"schema":1,"pid":-1,"instance":"wrong"}', 'not-json']) {
    const directory = await mkdtemp(join(tmpdir(), 'teacell-lease-'));
    try {
      const lock = join(directory, 'host.lock');
      await mkdir(lock);
      if (content !== null) await writeFile(join(lock, 'owner.json'), content);
      await assert.rejects(acquireLease(directory, randomUUID(), true));
      if (content !== null) assert.equal(await readFile(join(lock, 'owner.json'), 'utf8'), content);
    } finally { await rm(directory, { recursive: true, force: true }); }
  }
});

test('changed instance is not removed by former lease holder', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-lease-'));
  try {
    const release = await acquireLease(directory, randomUUID());
    const path = join(directory, 'host.lock', 'owner.json');
    const owner = JSON.parse(await readFile(path, 'utf8'));
    owner.instance = randomUUID();
    const altered = JSON.stringify(owner);
    await writeFile(path, altered);
    await assert.rejects(release(), /LEASE_CHANGED/);
    assert.equal(await readFile(path, 'utf8'), altered);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
