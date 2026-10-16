// Resource relocation experiment, not installer extraction or clean-machine QA.
import process from 'node:process';
import { cp, copyFile, readFile, readdir, mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { dirname, resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout, clearTimeout } from 'node:timers';
import { childEnvironment, PrototypeHost } from './supervisor.mjs';

if (process.platform !== 'win32' || process.arch !== 'x64') throw new Error('WINDOWS_X64_ONLY');
const source = dirname(fileURLToPath(import.meta.url));
const root = resolve(source, '../..');
const cache = join(root, 'artifacts/agent-runtime');
await mkdir(cache, { recursive: true });
const staging = await mkdtemp(join(cache, 'resource-stage-'));
const resources = join(staging, 'agent-poc');
await mkdir(resources);
const digest = async path => createHash('sha256').update(await readFile(path)).digest('hex');
const inventory = JSON.parse(await readFile(join(cache, 'dependency-inventory.json'), 'utf8'));
const runtimeManifest = JSON.parse(await readFile(join(source, 'runtime-manifest.json'), 'utf8'));
if (await digest(join(source, runtimeManifest.checksumFile)) !== runtimeManifest.checksumFileSha256) throw new Error('RUNTIME_CHECKSUM_SOURCE_MISMATCH');
if (inventory.links.length || await digest(join(source, 'package-lock.json')) !== inventory.lockSha256) throw new Error('INVENTORY_STALE');
const licenseManifest = JSON.parse(await readFile(join(source, 'licenses/manifest.json'), 'utf8'));
for (const license of licenseManifest.licenses) if (await digest(join(source, 'licenses', license.path)) !== license.sha256) throw new Error('LICENSE_DIGEST_MISMATCH');
await cp(join(source, 'node_modules'), join(resources, 'node_modules'), { recursive: true, force: false, errorOnExist: true });
await cp(join(source, 'licenses'), join(resources, 'licenses'), { recursive: true, force: false, errorOnExist: true });
for (const name of await readdir(source)) if (name.endsWith('.mjs') || ['package.json', 'package-lock.json', 'runtime-manifest.json', runtimeManifest.checksumFile].includes(name)) await copyFile(join(source, name), join(resources, name));
await copyFile(join(root, 'src-tauri/icons/Square30x30Logo.png'), join(resources, 'probe-preview.png'));
await copyFile(join(source, 'native-owner/target/debug/teacell-agent-owner-poc.exe'), join(resources, 'native-owner.exe'));
for (const asset of inventory.assets) if (await digest(join(resources, 'node_modules', asset.path)) !== asset.sha256) throw new Error('STAGED_ASSET_CHANGED');
for (const license of licenseManifest.licenses) if (await digest(join(resources, 'licenses', license.path)) !== license.sha256) throw new Error('STAGED_LICENSE_CHANGED');

async function run(executable, args, timeoutMs = 30000) {
  return await new Promise((resolveResult, reject) => {
    const child = spawn(executable, args, { cwd: staging, env: childEnvironment(process.env), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = ''; let stderr = '';
    const deadline = setTimeout(() => { child.kill(); reject(new Error('STAGED_PROCESS_TIMEOUT')); }, timeoutMs);
    child.once('error', reject);
    child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
    child.stdout.on('data', chunk => { stdout += chunk; if (stdout.length > 65536) child.kill(); });
    child.stderr.on('data', chunk => { stderr += chunk; if (stderr.length > 65536) child.kill(); });
    child.once('close', code => { clearTimeout(deadline); if (code !== 0) reject(new Error(`STAGED_PROCESS_FAILED:${stderr.slice(0, 2048)}`)); else resolveResult(stdout); });
  });
}
const powershell = join(process.env.SystemRoot, 'System32/WindowsPowerShell/v1.0/powershell.exe');
await run(powershell, ['-NoProfile', '-NonInteractive', '-File', join(source, 'stage-runtime.ps1'), '-Destination', join(staging, 'runtime')], 60000);
const node = join(staging, 'runtime/node-v24.19.0-win-x64/node.exe');
const probe = JSON.parse(await run(node, [join(resources, 'probe-relocated-runtime.mjs')]));
if (probe.ok !== true || probe.tools.length) throw new Error('STAGED_PROBE_FAILED');
const data = join(staging, 'job-probe-data');
await mkdir(data);
const host = new PrototypeHost(data, { executable: node, nativeOwner: join(resources, 'native-owner.exe'), entrypoint: join(resources, 'bootstrap.mjs') });
try {
  await host.ready();
  host.send({ type: 'prompt', runId: 'staged-probe', text: '打包目录进程屏障验证' });
  await host.wait(current => current.frames.some(frame => frame.type === 'settled' && frame.runId === 'staged-probe' && frame.aborted === false));
  if (host.frames.filter(frame => frame.type === 'delta').map(frame => frame.text).join('') !== '原型流式回复') throw new Error('STAGED_HOST_RESPONSE');
} finally { await host.close(); }
if (host.exitCode !== 0) throw new Error('STAGED_HOST_EXIT_FAILED');
const report = { schema: 1, recordedAt: new Date().toISOString(), staging, lockSha256: inventory.lockSha256,
  assetsVerified: inventory.assets.length, licenseFilesVerified: licenseManifest.licenses.length, probe,
  jobBarrier: { ready: true, completed: true, exitCode: host.exitCode },
  limitations: ['Resource copy with fresh official Node archive extraction; not installer extraction', 'Windows development machine only; not clean-machine or read-only install verification', 'No production GUI, model service, database or user media', 'Third-party license inventory is not yet complete'] };
const reportPath = join(cache, 'packaged-runtime-probe.json');
await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`);
process.stdout.write(`${reportPath}\n`);
