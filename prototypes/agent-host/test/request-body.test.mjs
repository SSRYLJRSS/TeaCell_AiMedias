import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath, URL } from 'node:url';
import { childEnvironment } from '../supervisor.mjs';

for (const size of [0, 1]) {
  test(`200000 chunks of ${size} bytes cannot retain an unbounded interrupt reaction list`, { timeout: 20000 }, async () => {
    const { stdout } = await promisify(execFile)(process.execPath, ['--expose-gc', fileURLToPath(new URL('../fixtures/body-fragments.mjs', import.meta.url)), String(size)], {
      env: childEnvironment(process.env), windowsHide: true, timeout: 15000, maxBuffer: 16384,
    });
    const sample = JSON.parse(stdout);
    assert.equal(sample.bytes, size * 200000);
    assert.equal(sample.reads, 200001);
    assert.equal(sample.timerRan, true);
    // Deliberate original implementations retained >100MiB while reading.
    // This observes live, post-GC references before the final read returns;
    // sampling only after return would miss the retention defect entirely.
    assert.ok(sample.live < 8 * 1024 * 1024, `retained heap: ${sample.live}`);
  });
}
