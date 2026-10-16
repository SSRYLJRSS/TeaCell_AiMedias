import process from 'node:process';
import { mkdir, writeFile, readFile, unlink, rmdir, rename, lstat, readdir } from 'node:fs/promises';
import { join } from 'node:path';

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

async function ownerOf(lock) {
  const info = await lstat(lock);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new Error('INVALID_LOCK');
  const entries = await readdir(lock);
  if (entries.length !== 1 || entries[0] !== 'owner.json') throw new Error('UNKNOWN_LOCK_CONTENT');
  const ownerPath = join(lock, 'owner.json');
  const ownerStat = await lstat(ownerPath);
  if (!ownerStat.isFile() || ownerStat.isSymbolicLink() || ownerStat.size > 2048) {
    throw new Error('INVALID_LOCK_OWNER');
  }
  const owner = JSON.parse(await readFile(ownerPath, 'utf8'));
  if (owner.schema !== 1 || !Number.isSafeInteger(owner.pid) || owner.pid <= 0 ||
      typeof owner.instance !== 'string' || !uuid.test(owner.instance)) throw new Error('INVALID_LOCK_OWNER');
  return owner;
}

export async function acquireLease(directory, instance, recover = false) {
  if (!uuid.test(instance)) throw new Error('INVALID_INSTANCE');
  const lock = join(directory, 'host.lock');
  try { await mkdir(lock); }
  catch (error) {
    if (error.code !== 'EEXIST' || !recover) throw error;
    // Serialize only explicit recovery attempts; never evict a live owner by PID.
    const guard = join(directory, 'recovery.lock');
    await mkdir(guard);
    try {
      const owner = await ownerOf(lock);
      try {
        process.kill(owner.pid, 0);
        throw new Error('LOCK_OWNER_ALIVE');
      } catch (probe) {
        if (probe.code !== 'ESRCH') throw probe;
      }
      const retired = join(directory, `host.retired.${owner.instance}`);
      await rename(lock, retired);
      // The detached directory contains exactly the previously checked record.
      await unlink(join(retired, 'owner.json'));
      await rmdir(retired);
      await mkdir(lock);
    } finally { await rmdir(guard); }
  }
  const record = { schema: 1, pid: process.pid, instance };
  try { await writeFile(join(lock, 'owner.json'), JSON.stringify(record), { flag: 'wx' }); }
  catch (error) {
    await rmdir(lock).catch(() => {});
    throw error;
  }
  return async () => {
    const owner = await ownerOf(lock);
    if (owner.pid !== process.pid || owner.instance !== instance) throw new Error('LEASE_CHANGED');
    await unlink(join(lock, 'owner.json'));
    await rmdir(lock);
  };
}
