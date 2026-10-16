import test from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, copyFile, readFile, writeFile, access, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout, clearTimeout } from 'node:timers';
import { childEnvironment } from '../supervisor.mjs';

const source = fileURLToPath(new URL('..', import.meta.url));
test('runtime target pins agree with the recorded vendor checksum file', async () => {
  const manifest = JSON.parse(await readFile(join(source, 'runtime-manifest.json'), 'utf8'));
  assert.deepEqual(Object.keys(manifest.targets).sort(), ['aarch64-apple-darwin', 'x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu']);
  const checksums = await readFile(join(source, manifest.checksumFile));
  assert.equal(createHash('sha256').update(checksums).digest('hex'), manifest.checksumFileSha256);
  const lines = checksums.toString('utf8').trim().split(/\r?\n/).map(line => line.split(/\s+/));
  for (const target of Object.values(manifest.targets)) {
    const url = new URL(target.url);
    assert.equal(url.origin, 'https://nodejs.org');
    assert.equal(url.pathname.split('/')[2], `v${manifest.nodeVersion}`);
    assert.equal(lines.find(([, name]) => name === url.pathname.split('/').at(-1))?.[0], target.sha256);
  }
});

async function fixture(t) {
  const directory = await mkdtemp(join(tmpdir(), 'teacell-runtime-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const prototype = join(directory, 'prototypes/agent-host');
  await mkdir(prototype, { recursive: true });
  await copyFile(join(source, 'prepare-runtime.ps1'), join(prototype, 'prepare-runtime.ps1'));
  const manifest = JSON.parse(await readFile(join(source, 'runtime-manifest.json'), 'utf8'));
  const manifestPath = join(prototype, 'runtime-manifest.json');
  await writeFile(manifestPath, JSON.stringify(manifest));
  return { directory, prototype, manifest, manifestPath, cache: join(directory, 'artifacts/agent-runtime') };
}
async function prepare(f, target, downloadOnly = false) {
  const powershell = join(process.env.SystemRoot, 'System32/WindowsPowerShell/v1.0/powershell.exe');
  return await new Promise((resolve, reject) => {
    const child = spawn(powershell, ['-NoProfile', '-NonInteractive', '-File', join(f.prototype, 'prepare-runtime.ps1'), '-Target', target, ...(downloadOnly ? ['-DownloadOnly'] : [])], { env: childEnvironment(process.env), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = ''; let stderr = '';
    const timer = setTimeout(() => { child.kill(); reject(new Error('RUNTIME_PREPARATION_TIMEOUT')); }, 10000);
    child.once('error', reject);
    child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
    child.stdout.on('data', chunk => { stdout += chunk; if (stdout.length > 65536) child.kill(); });
    child.stderr.on('data', chunk => { stderr += chunk; if (stderr.length > 65536) child.kill(); });
    child.once('close', code => { clearTimeout(timer); resolve({ code, stdout, stderr }); });
  });
}
const windows = { skip: process.platform !== 'win32', timeout: 15000 };
test('runtime preparation refuses an unknown target before creating the cache', windows, async t => {
  const f = await fixture(t);
  const result = await prepare(f, 'unknown-target');
  assert.notEqual(result.code, 0);
  assert.match(result.stderr, /Unverified prototype runtime target/);
  await assert.rejects(access(f.cache), error => error.code === 'ENOENT');
});
test('runtime preparation refuses foreign execution before download or extraction', windows, async t => {
  const f = await fixture(t);
  const result = await prepare(f, 'aarch64-apple-darwin');
  assert.notEqual(result.code, 0);
  assert.match(result.stderr, /matching native host/);
  await assert.rejects(access(f.cache), error => error.code === 'ENOENT');
});
test('a corrupt cached foreign archive is rejected without extracting it', windows, async t => {
  const f = await fixture(t);
  const target = f.manifest.targets['x86_64-unknown-linux-gnu'];
  await mkdir(f.cache, { recursive: true });
  await writeFile(join(f.cache, new URL(target.url).pathname.split('/').at(-1)), 'public-corrupt-fixture');
  const result = await prepare(f, 'x86_64-unknown-linux-gnu', true);
  assert.notEqual(result.code, 0);
  assert.match(result.stderr, /SHA256 mismatch/);
  await assert.rejects(access(join(f.cache, 'node-v24.19.0-linux-x64')), error => error.code === 'ENOENT');
});
test('download-only verifies fixture bytes but does not extract or execute a foreign runtime', windows, async t => {
  const f = await fixture(t);
  const target = f.manifest.targets['aarch64-apple-darwin'];
  const bytes = 'public-fixture-for-checksum-only';
  target.sha256 = createHash('sha256').update(bytes).digest('hex');
  await writeFile(f.manifestPath, JSON.stringify(f.manifest));
  await mkdir(f.cache, { recursive: true });
  const archive = join(f.cache, new URL(target.url).pathname.split('/').at(-1));
  await writeFile(archive, bytes);
  const result = await prepare(f, 'aarch64-apple-darwin', true);
  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stdout.trim(), archive);
  await assert.rejects(access(join(f.cache, 'node-v24.19.0-darwin-arm64')), error => error.code === 'ENOENT');
});
