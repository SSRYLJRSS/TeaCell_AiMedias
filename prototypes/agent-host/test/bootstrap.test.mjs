import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import { childEnvironment } from '../supervisor.mjs';

for (const scenario of ['wrong', 'EOF', 'timeout']) {
  test(`bootstrap ${scenario} cannot load SDK or acquire library lease`, { timeout: 10000 }, async () => {
    const directory = await mkdtemp(join(tmpdir(), 'teacell-bootstrap-'));
    const child = spawn(process.execPath, [fileURLToPath(new URL('../bootstrap.mjs', import.meta.url)),
      directory, randomUUID()], { env: childEnvironment(process.env), windowsHide: true,
      stdio: ['pipe', 'pipe', 'pipe'] });
    let outputBytes = 0;
    child.stdout.on('data', bytes => { outputBytes += bytes.length; });
    child.stderr.resume();
    const exited = once(child, 'exit');
    try {
      if (scenario === 'wrong') child.stdin.end('wrong\n');
      if (scenario === 'EOF') child.stdin.end();
      const [code] = await exited;
      assert.equal(code, 2);
      assert.equal(outputBytes, 0);
      await assert.rejects(stat(join(directory, 'host.lock')), { code: 'ENOENT' });
    } finally {
      child.kill('SIGKILL');
      await rm(directory, { recursive: true, force: true });
    }
  });
}
