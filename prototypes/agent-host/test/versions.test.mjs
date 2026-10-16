import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { URL } from 'node:url';

test('lockfile pins all same-release Pi runtime packages and integrity', async () => {
  const lock = JSON.parse(await readFile(new URL('../package-lock.json', import.meta.url), 'utf8'));
  let count = 0;
  for (const [path, entry] of Object.entries(lock.packages)) {
    if (!path.includes('/@earendil-works/')) continue;
    count += 1;
    assert.equal(entry.version, '1.0.4', path);
    assert.ok(entry.integrity.startsWith('sha512-'), path);
    assert.ok(entry.resolved.startsWith('https://registry.npmjs.org/'), path);
  }
  assert.ok(count >= 7);
});
