import process from 'node:process';
import { readdir, stat, readFile, mkdtemp, rm } from 'node:fs/promises';
import { join, dirname, extname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { performance } from 'node:perf_hooks';
import { PrototypeHost } from './supervisor.mjs';

const root = dirname(fileURLToPath(import.meta.url));
async function inventory(directory) {
  const files = [];
  async function visit(path) {
    for (const entry of await readdir(path, { withFileTypes: true })) {
      const full = join(path, entry.name);
      if (entry.isSymbolicLink()) throw new Error('ASSET_LINK_REQUIRES_REVIEW');
      if (entry.isDirectory()) await visit(full);
      else if (entry.isFile()) files.push({ path: full.slice(directory.length + 1), bytes: (await stat(full)).size });
    }
  }
  await visit(directory);
  return { fileCount: files.length, bytes: files.reduce((total, file) => total + file.bytes, 0),
    runtimeAssets: files.filter(file => ['.wasm', '.node', '.exe', '.dll'].includes(extname(file.path))) };
}

const directory = await mkdtemp(join(tmpdir(), 'teacell-measure-'));
const samples = [];
try {
  for (let index = 0; index < 10; index += 1) {
    const start = performance.now();
    const host = new PrototypeHost(directory, { nativeOwner: process.env.TEACELL_NATIVE_OWNER });
    try {
      await host.ready();
      const readyMs = performance.now() - start;
      host.send({ type: 'measure' });
      await host.wait(h => h.frames.some(frame => frame.type === 'measurement'));
      const measurement = host.frames.find(frame => frame.type === 'measurement');
      const exitStart = performance.now();
      await host.close();
      samples.push({ readyMs, exitMs: performance.now() - exitStart,
        memory: measurement.memory, cpu: measurement.cpu });
    } finally { await host.close(); }
  }
  const lockBytes = await readFile(join(root, 'package-lock.json'));
  const runtimeDir = dirname(process.execPath);
  const data = {
    schemaVersion: 1, measuredAt: new Date().toISOString(), node: process.version,
    platform: process.platform, arch: process.arch, mode: process.env.TEACELL_NATIVE_OWNER ? 'Windows Job + SDK' : 'Node supervisor + SDK',
    buildConditions: 'development PoC, mock model, unfiltered dependency assets, concurrent workloads not controlled',
    lockSha256: createHash('sha256').update(lockBytes).digest('hex'),
    dependencies: await inventory(join(root, 'node_modules')),
    runtime: await inventory(runtimeDir), samples,
    installerDelta: null, approvedBudget: null,
  };
  process.stdout.write(JSON.stringify(data, null, 2) + '\n');
} finally { await rm(directory, { recursive: true, force: true }); }
