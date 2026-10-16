import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { once } from 'node:events';
import { fileURLToPath, URL } from 'node:url';
import { PrototypeHost } from '../supervisor.mjs';

for (const [scenario, message] of [
  ['sdk', /HANDSHAKE_MISMATCH/], ['instance', /INVALID_EVENT/],
  ['protocol', /INVALID_EVENT/], ['duplicate', /INVALID_EVENT/],
  ['pollution', /Unexpected token/], ['flood', /FRAME_TOO_LARGE/],
  ['silent', /HOST_TIMEOUT/],
]) {
  test(`supervisor rejects ${scenario} and actually reaps the child`, { timeout: 15000 }, async () => {
    const directory = await mkdtemp(join(tmpdir(), 'teacell-fault-'));
    const host = new PrototypeHost(directory, {
      entrypoint: fileURLToPath(new URL('./fixtures/fault-host.mjs', import.meta.url)),
      faultScenario: scenario,
    });
    try {
      const closed = once(host.child, 'close');
      if (scenario === 'silent') await assert.rejects(host.wait(h => h.frames.length > 0, 1000), message);
      else if (scenario === 'duplicate') {
        await assert.rejects((async () => { await host.ready(); await host.wait(() => false, 1000); })(), message);
      } else await assert.rejects(host.ready(), message);
      await closed;
      assert.equal(host.closed, true);
      assert.ok(host.failure);
      assert.ok(host.frames.length <= 1);
    } finally { await host.close(); await rm(directory, { recursive: true, force: true }); }
  });
}
