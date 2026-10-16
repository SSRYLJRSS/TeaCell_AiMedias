import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, copyFile, readFile, writeFile, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import { childEnvironment } from '../supervisor.mjs';
import { setTimeout, clearTimeout } from 'node:timers';

const source = fileURLToPath(new URL('..', import.meta.url));
for (const [scenario, expected] of [['digest', 'LICENSE_DIGEST_MISMATCH'], ['version', 'LICENSE_PACKAGE_VERSION_MISMATCH'], ['missing', 'PI_LICENSE_RESOURCE_REQUIRED']]) {
  test(`package overlay refuses ${scenario} license evidence before writing configuration`, { timeout: 15000 }, async t => {
    const directory = await mkdtemp(join(tmpdir(), 'teacell-license-'));
    t.after(() => rm(directory, { recursive: true, force: true }));
    const prototype = join(directory, 'prototypes', 'agent-host');
    await mkdir(join(prototype, 'licenses'), { recursive: true });
    for (const name of ['package-experiment.mjs', 'package-lock.json', 'runtime-manifest.json']) await copyFile(join(source, name), join(prototype, name));
    const manifest = JSON.parse(await readFile(join(source, 'licenses', 'manifest.json'), 'utf8'));
    for (const license of manifest.licenses) await copyFile(join(source, 'licenses', license.path), join(prototype, 'licenses', license.path));
    if (scenario === 'digest') await writeFile(join(prototype, 'licenses', manifest.licenses[0].path), 'altered-public-fixture');
    if (scenario === 'version') manifest.licenses[0].packages[0].version = '0.0.0-invalid';
    if (scenario === 'missing') manifest.licenses = [];
    await writeFile(join(prototype, 'licenses', 'manifest.json'), JSON.stringify(manifest));
    const result = await new Promise((resolve, reject) => {
      const child = spawn(process.execPath, [join(prototype, 'package-experiment.mjs')], { env: childEnvironment(process.env), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
      const timeout = setTimeout(() => { child.kill(); reject(new Error('PACKAGE_GUARD_TIMEOUT')); }, 5000);
      let stderr = '';
      child.stdout.resume(); child.stderr.setEncoding('utf8');
      child.stderr.on('data', chunk => { stderr += chunk; if (stderr.length > 65536) child.kill(); });
      child.once('error', reject);
      child.once('close', code => { clearTimeout(timeout); resolve({ code, stderr }); });
    });
    assert.notEqual(result.code, 0);
    assert.ok(result.stderr.includes(expected), result.stderr);
    await assert.rejects(access(join(directory, 'artifacts', 'agent-runtime', 'bundle-poc.conf.json')), error => error.code === 'ENOENT');
  });
}
