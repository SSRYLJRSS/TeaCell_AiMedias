#!/usr/bin/env node
/* global process, console, URL, fetch */
import { createHash, randomUUID } from 'node:crypto';
import { Buffer } from 'node:buffer';
import { execFileSync } from 'node:child_process';
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadWindowsVCRuntime } from './windows-vc-runtime.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const tauri = join(root, 'src-tauri');
const target = 'x86_64-pc-windows-msvc';
const maxAgentBytes = 250 * 1024 * 1024;
const runtimeDirectory = join(root, 'artifacts', 'agent-runtime');
const fail = (message) => { throw new Error(message); };
const sha256 = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');

function regularFile(path, label) {
  let info;
  try { info = lstatSync(path); } catch { fail(`${label}缺失：${path}`); }
  if (!info.isFile() || info.isSymbolicLink()) fail(`${label}不是普通文件：${path}`);
  return info;
}

function sha256Buffer(value) {
  return createHash('sha256').update(value).digest('hex');
}

function pinnedNodeUrl(value, label) {
  let url;
  try { url = new URL(value); } catch { fail(`${label} URL无效`); }
  if (url.protocol !== 'https:' || url.hostname !== 'nodejs.org' || url.username || url.password || url.search || url.hash) {
    fail(`${label}必须来自固定的Node官方HTTPS地址`);
  }
  return url;
}

async function ensurePinnedDownload(urlText, destination, expectedSha256, maxBytes, label) {
  pinnedNodeUrl(urlText, label);
  if (existsSync(destination)) {
    regularFile(destination, label);
    if (sha256(destination) !== expectedSha256) fail(`${label}缓存摘要不匹配；已保留原文件：${destination}`);
    return;
  }
  const response = await fetch(urlText, { redirect: 'follow' });
  if (!response.ok) fail(`${label}下载失败：HTTP ${response.status}`);
  const finalUrl = new URL(response.url);
  if (finalUrl.protocol !== 'https:' || finalUrl.hostname !== 'nodejs.org') fail(`${label}重定向离开Node官方主机`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length === 0 || bytes.length > maxBytes) fail(`${label}下载大小越界：${bytes.length} bytes`);
  if (sha256Buffer(bytes) !== expectedSha256) fail(`${label}下载摘要与锁定值不匹配`);
  mkdirSync(dirname(destination), { recursive: true });
  try {
    writeFileSync(destination, bytes, { flag: 'wx' });
  } catch (error) {
    if (error?.code !== 'EEXIST') throw error;
    regularFile(destination, label);
    if (sha256(destination) !== expectedSha256) fail(`${label}并发缓存摘要不匹配；已保留原文件`);
  }
}

function extractPinnedWindowsRuntime(archive, runtime, nodeVersion) {
  const executable = join(runtimeDirectory, runtime.executable.replaceAll('/', sep));
  const license = join(dirname(executable), 'LICENSE');
  const executablePresent = existsSync(executable);
  const licensePresent = existsSync(license);
  if (executablePresent || licensePresent) {
    if (!executablePresent || !licensePresent) fail(`Node解包目录不完整；已保留现场：${dirname(executable)}`);
    return;
  }
  if (existsSync(dirname(executable))) fail(`Node解包目标已存在但不完整；已保留现场：${dirname(executable)}`);

  const extraction = join(runtimeDirectory, `.extract-${process.pid}-${randomUUID()}`);
  mkdirSync(extraction, { recursive: false });
  try {
    const command = '$ErrorActionPreference = "Stop"; Expand-Archive -LiteralPath $env:AGENT_NODE_ARCHIVE -DestinationPath $env:AGENT_NODE_DESTINATION';
    execFileSync('powershell.exe', ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', command], {
      env: { ...process.env, AGENT_NODE_ARCHIVE: archive, AGENT_NODE_DESTINATION: extraction },
      stdio: 'inherit',
    });
    const extractedRoot = join(extraction, `node-v${nodeVersion}-win-x64`);
    regularFile(join(extractedRoot, 'node.exe'), '解包后的Node可执行文件');
    regularFile(join(extractedRoot, 'LICENSE'), '解包后的Node许可证');
    const destination = dirname(executable);
    mkdirSync(dirname(destination), { recursive: true });
    renameSync(extractedRoot, destination);
  } finally {
    rmSync(extraction, { recursive: true, force: true });
  }
}

function ensureHostDependencies(packageRoot) {
  const dependencies = ['@earendil-works/pi-coding-agent', '@earendil-works/pi-ai'];
  let installLock;
  try { installLock = JSON.parse(readFileSync(join(packageRoot, 'node_modules', '.package-lock.json'), 'utf8')); }
  catch { installLock = null; }
  const ready = installLock?.lockfileVersion === 3 && dependencies.every((id) => {
    const path = join(packageRoot, 'node_modules', id, 'package.json');
    if (!existsSync(path)) return false;
    try { return JSON.parse(readFileSync(path, 'utf8')).version === '1.0.4'; }
    catch { return false; }
  });
  if (ready) return;

  const args = ['ci', '--prefix', packageRoot, '--ignore-scripts', '--no-audit', '--no-fund'];
  const npmCli = process.env.npm_execpath;
  if (npmCli) {
    execFileSync(process.execPath, [npmCli, ...args], { cwd: root, stdio: 'inherit' });
    return;
  }
  const command = '& $env:AGENT_NPM_COMMAND ci --prefix $env:AGENT_HOST_PACKAGE --ignore-scripts --no-audit --no-fund';
  execFileSync('powershell.exe', ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', command], {
    cwd: root,
    stdio: 'inherit',
    env: {
      ...process.env,
      AGENT_NPM_COMMAND: 'npm.cmd',
      AGENT_HOST_PACKAGE: packageRoot,
    },
  });
}

function collectFiles(directory) {
  const files = [];
  if (!existsSync(directory)) return files;
  const visit = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const path = join(current, entry.name);
      const info = lstatSync(path);
      if (info.isSymbolicLink()) fail(`Agent Host资源不能包含链接：${path}`);
      if (info.isDirectory()) visit(path);
      else if (info.isFile()) files.push(path);
      else fail(`Agent Host资源类型不受支持：${path}`);
    }
  };
  visit(directory);
  return files;
}

function addTree(resources, source, destination) {
  if (!collectFiles(source).length) fail(`Agent Host资源目录为空：${source}`);
  resources[resolve(source)] = destination;
}

function addFile(resources, source, destination) {
  regularFile(source, '应用打包资源');
  resources[resolve(source)] = destination.replaceAll(sep, '/');
}

function addLegacyResources(resources) {
  const sidecar = JSON.parse(readFileSync(join(tauri, 'tauri.sidecar.conf.json'), 'utf8'));
  for (const source of sidecar.bundle.externalBin ?? []) {
    // externalBin is kept as-is in the generated overlay; this loop documents its source check.
    if (typeof source !== 'string') fail('sidecar externalBin 配置无效');
  }
  const binariesLicenses = join(tauri, 'binaries', 'licenses');
  for (const file of collectFiles(binariesLicenses).filter((path) => path.toLowerCase().endsWith('.txt'))) {
    addFile(resources, file, relative(tauri, file));
  }
  const heifRoot = join(tauri, 'native', 'heif');
  for (const file of collectFiles(heifRoot).filter((path) => path.split(sep).includes('licenses'))) {
    addFile(resources, file, relative(tauri, file));
  }
  for (const name of ['LICENSE', 'NOTICE']) addFile(resources, join(root, name), name);
  const notices = join(root, 'LICENSES');
  for (const file of collectFiles(notices).filter((path) => path.toLowerCase().endsWith('.txt'))) {
    addFile(resources, file, `LICENSES/${relative(notices, file).replaceAll(sep, '/')}`);
  }
  return sidecar.bundle.externalBin ?? [];
}

function validateRuntime(runtimeManifest) {
  if (runtimeManifest.schemaVersion !== 1 || runtimeManifest.nodeVersion !== '24.19.0') {
    fail('Agent Host运行时版本未获批准');
  }
  const runtime = runtimeManifest.targets?.[target];
  if (!runtime || !/^[a-f0-9]{64}$/.test(runtime.sha256 ?? '') ||
      !/^[a-f0-9]{64}$/.test(runtime.executableSha256 ?? '') ||
      !/^[a-f0-9]{64}$/.test(runtime.licenseSha256 ?? '')) {
    fail(`Agent Host运行时manifest无效：${target}`);
  }
  if (basename(runtimeManifest.checksumFile) !== runtimeManifest.checksumFile) fail('Node官方校验和文件名无效');
  const sums = join(runtimeDirectory, runtimeManifest.checksumFile);
  regularFile(sums, 'Node官方校验和');
  if (sha256(sums) !== runtimeManifest.checksumFileSha256) fail('Node官方校验和文件摘要不匹配');
  const zipName = runtime.url.split('/').at(-1);
  const officialLine = readFileSync(sums, 'utf8').split(/\r?\n/u).find((line) => line.endsWith(`  ${zipName}`));
  if (!officialLine || officialLine.slice(0, 64) !== runtime.sha256) fail('Node归档摘要与官方校验和不匹配');

  const archive = join(runtimeDirectory, zipName);
  regularFile(archive, 'Node官方归档');
  if (sha256(archive) !== runtime.sha256) fail('Node官方归档SHA256不匹配');
  const node = join(runtimeDirectory, runtime.executable.replaceAll('/', sep));
  const license = join(dirname(node), 'LICENSE');
  regularFile(node, 'Node可执行文件');
  regularFile(license, 'Node许可证');
  if (sha256(node) !== runtime.executableSha256) fail('Node可执行文件SHA256不匹配');
  if (sha256(license) !== runtime.licenseSha256) fail('Node许可证SHA256不匹配');
  let version;
  try {
    version = execFileSync(node, ['--version'], { encoding: 'utf8', env: { PATH: '', SystemRoot: process.env.SystemRoot ?? '' } }).trim();
  } catch { fail('Node随包运行时无法执行'); }
  if (version !== `v${runtimeManifest.nodeVersion}`) fail(`Node运行时版本不匹配：${version}`);
  return { node, license };
}

function validatePackage(packageRoot) {
  const packageInfo = JSON.parse(readFileSync(join(packageRoot, 'package.json'), 'utf8'));
  const lock = JSON.parse(readFileSync(join(packageRoot, 'package-lock.json'), 'utf8'));
  if (packageInfo.dependencies?.['@earendil-works/pi-coding-agent'] !== '1.0.4' ||
      packageInfo.dependencies?.['@earendil-works/pi-ai'] !== '1.0.4' ||
      lock.packages?.['node_modules/@earendil-works/pi-coding-agent']?.version !== '1.0.4' ||
      lock.packages?.['node_modules/@earendil-works/pi-ai']?.version !== '1.0.4') {
    fail('Pi SDK版本或锁文件不符合A0-02基线');
  }
  for (const id of ['@earendil-works/pi-coding-agent', '@earendil-works/pi-ai']) {
    const dependency = join(packageRoot, 'node_modules', id, 'package.json');
    regularFile(dependency, `Pi依赖 ${id}`);
    if (JSON.parse(readFileSync(dependency, 'utf8')).version !== '1.0.4') fail(`Pi依赖版本不匹配：${id}`);
  }
  const files = collectFiles(packageRoot);
  const bytes = files.reduce((total, path) => total + lstatSync(path).size, 0);
  return { files, bytes };
}

function createOverlay(externalBin, hostPackage, runtimeFiles, vcRuntime) {
  const resources = {};
  addLegacyResources(resources);
  addTree(resources, hostPackage, 'agent-host/host');
  addFile(resources, runtimeFiles.node, 'agent-host/runtime/node-v24.19.0-win-x64/node.exe');
  addFile(resources, runtimeFiles.license, 'agent-host/runtime/node-v24.19.0-win-x64/LICENSE');
  for (const file of vcRuntime.files) addFile(resources, file.path, file.name);
  const content = `${JSON.stringify({ bundle: { externalBin, resources } }, null, 2)}\n`;
  const digest = createHash('sha256').update(content).digest('hex').slice(0, 16);
  const directory = join(root, 'artifacts', 'agent-host-build');
  mkdirSync(directory, { recursive: true });
  const output = join(directory, `tauri-overlay-${digest}.json`);
  if (existsSync(output) && readFileSync(output, 'utf8') !== content) fail('Agent Host overlay内容与路径摘要不一致');
  if (!existsSync(output)) writeFileSync(output, content, { flag: 'wx' });
  return output;
}

async function main() {
  const args = process.argv.slice(2);
  const selected = args.indexOf('--target');
  if (selected < 0 || args[selected + 1] !== target) fail(`Agent Host当前仅支持 --target ${target}`);
  if (process.platform !== 'win32' || process.arch !== 'x64') fail('Agent Host打包必须由Windows x64原生runner执行');
  const packageRoot = join(root, 'packages', 'agent-host');
  const runtimeManifest = JSON.parse(readFileSync(join(packageRoot, 'runtime-manifest.json'), 'utf8'));
  if (runtimeManifest.schemaVersion !== 1 || runtimeManifest.nodeVersion !== '24.19.0' ||
      !/^[a-f0-9]{64}$/.test(runtimeManifest.checksumFileSha256 ?? '')) {
    fail('Agent Host运行时manifest或官方校验和摘要无效');
  }
  const runtime = runtimeManifest.targets?.[target];
  if (!runtime || !/^[a-f0-9]{64}$/.test(runtime.sha256 ?? '') ||
      !/^[a-f0-9]{64}$/.test(runtime.executableSha256 ?? '') ||
      !/^[a-f0-9]{64}$/.test(runtime.licenseSha256 ?? '') ||
      typeof runtime.executable !== 'string' || runtime.executable.startsWith('/') || runtime.executable.split(/[\\/]/u).includes('..')) {
    fail(`Agent Host运行时manifest无效：${target}`);
  }
  if (basename(runtimeManifest.checksumFile) !== runtimeManifest.checksumFile) fail('Node官方校验和文件名无效');
  const archiveName = basename(pinnedNodeUrl(runtime.url, 'Node官方归档').pathname);
  await ensurePinnedDownload(
    runtimeManifest.checksumSource,
    join(runtimeDirectory, runtimeManifest.checksumFile),
    runtimeManifest.checksumFileSha256,
    2 * 1024 * 1024,
    'Node官方校验和',
  );
  const archive = join(runtimeDirectory, archiveName);
  await ensurePinnedDownload(runtime.url, archive, runtime.sha256, 80 * 1024 * 1024, 'Node官方归档');
  extractPinnedWindowsRuntime(archive, runtime, runtimeManifest.nodeVersion);
  ensureHostDependencies(packageRoot);
  const runtimeFiles = validateRuntime(runtimeManifest);
  const vcRuntime = loadWindowsVCRuntime();
  const packageSize = validatePackage(packageRoot);
  const totalBytes = packageSize.bytes + lstatSync(runtimeFiles.node).size + lstatSync(runtimeFiles.license).size;
  if (totalBytes > maxAgentBytes) fail(`Agent Host资源超过250MiB上限：${totalBytes} bytes`);
  const sidecar = JSON.parse(readFileSync(join(tauri, 'tauri.sidecar.conf.json'), 'utf8'));
  const output = createOverlay(sidecar.bundle.externalBin ?? [], packageRoot, runtimeFiles, vcRuntime);
  console.log(JSON.stringify({
    overlay: output,
    agentFileCount: packageSize.files.length + 2,
    agentResourceBytes: totalBytes,
    vcRuntimeVersion: vcRuntime.version,
    vcRuntimeFileCount: vcRuntime.files.length,
    vcRuntimeBytes: vcRuntime.files.reduce((total, file) => total + file.bytes, 0),
    vcRuntimePackageSha256: vcRuntime.packageSha256,
    vcRuntimeFileSha256s: Object.fromEntries(vcRuntime.files.map(file => [file.name, file.sha256])),
  }));
}

main().catch((error) => {
  console.error(`[agent-host] ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 1;
});
