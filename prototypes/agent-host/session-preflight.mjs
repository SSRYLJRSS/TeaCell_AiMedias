// Structural experiment for pinned Pi v3 transcripts, not a production schema
// validator, migration engine, directory grant, or hostile-filesystem sandbox.
import { open, lstat, realpath } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { Buffer } from 'node:buffer';
import { TextDecoder } from 'node:util';

export async function preflightSession(file, sessions) {
  if (dirname(resolve(file)) !== resolve(sessions)) throw new Error('SESSION_PATH_DENIED');
  const metadata = await lstat(file);
  if (!metadata.isFile() || metadata.isSymbolicLink()) throw new Error('SESSION_PATH_DENIED');
  if (dirname(await realpath(file)) !== await realpath(sessions)) throw new Error('SESSION_PATH_DENIED');
  const handle = await open(file, 'r');
  try {
    const limit = 16 * 1024 * 1024;
    const buffer = Buffer.alloc(limit + 1);
    let length = 0;
    while (length < buffer.length) {
      const { bytesRead } = await handle.read(buffer, length, buffer.length - length, null);
      if (!bytesRead) break;
      length += bytesRead;
    }
    if (length > limit) throw new Error('SESSION_SIZE_LIMIT');
    if (!length || buffer[length - 1] !== 10) throw new Error('SESSION_TRUNCATED');
    let entries;
    try {
      const text = new TextDecoder('utf-8', { fatal: true }).decode(buffer.subarray(0, length));
      entries = text.trimEnd().split('\n').map(line => JSON.parse(line));
    } catch { throw new Error('SESSION_CORRUPT'); }
    const [header, ...records] = entries;
    if (header?.type !== 'session' || header.version !== 3 || typeof header.id !== 'string' || !header.id) throw new Error('SESSION_VERSION_UNSUPPORTED');
    const ids = new Set();
    for (const record of records) {
      if (!record || typeof record !== 'object' || typeof record.type !== 'string' || record.type === 'session'
        || typeof record.id !== 'string' || !record.id || ids.has(record.id)
        || !(record.parentId === null || ids.has(record.parentId))) throw new Error('SESSION_CORRUPT');
      ids.add(record.id);
    }
    return { id: header.id, entries: records.length };
  } finally { await handle.close(); }
}
