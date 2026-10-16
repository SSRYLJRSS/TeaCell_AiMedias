/* global process */
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync, constants as fsConstants } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const WINDOWS_VC_RUNTIME_FILES = Object.freeze([
  'concrt140.dll',
  'msvcp140.dll',
  'msvcp140_1.dll',
  'msvcp140_2.dll',
  'msvcp140_atomic_wait.dll',
  'msvcp140_codecvt_ids.dll',
  'vccorlib140.dll',
  'vcruntime140.dll',
  'vcruntime140_1.dll',
  'vcruntime140_threads.dll',
]);

export const WINDOWS_VC_RUNTIME_PIN = Object.freeze({
  version: '14.44.35211.0',
  packageSha256: 'cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b',
  files: Object.freeze({
    'concrt140.dll': Object.freeze({ bytes: 324208, sha256: '2405355f0a58067b258f8df33c327e3a3d716eaac5a3a5aebb757842d85bd376' }),
    'msvcp140.dll': Object.freeze({ bytes: 557728, sha256: '0f885b509a685d2bbfa652fed26b5fb31d88fbdab0a978c641d1c7b8aa460aa9' }),
    'msvcp140_1.dll': Object.freeze({ bytes: 35952, sha256: 'bfad5aef4c63a669e3c140655cdfdf395b6c979b400a447bd5dcb65ed8826c3d' }),
    'msvcp140_2.dll': Object.freeze({ bytes: 280200, sha256: '3ea06f0ee098b4823cb79599df3780e7f23cce52c19aac31d2a0d47efe33a5e9' }),
    'msvcp140_atomic_wait.dll': Object.freeze({ bytes: 50304, sha256: '640b2aefced484d0368eea5bdd06addd0658a3a70a49256e560d6923b404a479' }),
    'msvcp140_codecvt_ids.dll': Object.freeze({ bytes: 31872, sha256: 'f2069a52880ec885ee7f0511186100eb7fada0411a2b4948fafea7735b878a18' }),
    'vccorlib140.dll': Object.freeze({ bytes: 352384, sha256: '19839407c3fdbc824e5bce189bf68ddf8097f12ec28b757797ffa0415c144ddd' }),
    'vcruntime140.dll': Object.freeze({ bytes: 124544, sha256: 'd5e4d9a3e835fa679450145d6a7d94e36573a509317111904d9b3712c30d9066' }),
    'vcruntime140_1.dll': Object.freeze({ bytes: 49792, sha256: '1f2d41c4aa5db0bc33ebf7b66d72943a817d7ce6cbe880502a9403823633093f' }),
    'vcruntime140_threads.dll': Object.freeze({ bytes: 38528, sha256: '219915cf20822f34d5e7c1fdd4e21ae7f3396881096c51036225fb8f84b47afa' }),
  }),
});

const fail = (message) => { throw new Error(`[vc-runtime] ${message}`); };
const sha256 = (value) => createHash('sha256').update(value).digest('hex');
const isSha256 = (value) => typeof value === 'string' && /^[a-f0-9]{64}$/iu.test(value);
const isMicrosoftSigner = (value) => typeof value === 'string' && /(?:^|,\s*)O=Microsoft Corporation(?:,|$)/iu.test(value);

export function validateVCRuntimeInventory(inventory, pin = WINDOWS_VC_RUNTIME_PIN) {
  if (inventory?.schemaVersion !== 1 || inventory.version !== pin.version ||
      !/^14\.\d+\.\d+\.\d+$/u.test(inventory.version)) {
    fail('Microsoft VC Runtime inventory version or schema is invalid');
  }
  const packageInfo = inventory.package;
  if (packageInfo?.version !== inventory.version || packageInfo.signatureStatus !== 'Valid' ||
      !isMicrosoftSigner(packageInfo.signer) || !isSha256(packageInfo.sha256) ||
      packageInfo.sha256.toLowerCase() !== pin.packageSha256 ||
      !Number.isSafeInteger(packageInfo.bytes) || packageInfo.bytes <= 0) {
    fail('Microsoft VC Redistributable package signature, version, or SHA256 is invalid');
  }
  if (!Array.isArray(inventory.files) || inventory.files.length !== WINDOWS_VC_RUNTIME_FILES.length) {
    fail('Microsoft VC Runtime inventory must contain the complete x64 CRT file set');
  }
  const filesByName = new Map(inventory.files.map(file => [file?.name?.toLowerCase(), file]));
  if (filesByName.size !== WINDOWS_VC_RUNTIME_FILES.length ||
      WINDOWS_VC_RUNTIME_FILES.some(name => !filesByName.has(name))) {
    fail('Microsoft VC Runtime inventory has missing, duplicate, or unexpected files');
  }
  const files = WINDOWS_VC_RUNTIME_FILES.map(name => filesByName.get(name));
  for (const file of files) {
    const expected = pin.files[file.name.toLowerCase()];
    if (file.version !== inventory.version || file.signatureStatus !== 'Valid' ||
        !isMicrosoftSigner(file.signer) || !isSha256(file.sha256) ||
        !expected || file.bytes !== expected.bytes || file.sha256.toLowerCase() !== expected.sha256 ||
        !Number.isSafeInteger(file.bytes) || file.bytes <= 0 || typeof file.path !== 'string') {
      fail(`Microsoft VC Runtime file is untrusted or inconsistent: ${file.name}`);
    }
    const info = lstatSync(file.path);
    if (!info.isFile() || info.isSymbolicLink() || info.size !== file.bytes ||
        sha256(readFileSync(file.path)) !== file.sha256.toLowerCase()) {
      fail(`Microsoft VC Runtime file changed after signature inspection: ${file.name}`);
    }
  }
  return { version: inventory.version, package: packageInfo, files };
}

export function stageVCRuntimeInventory(validated, repositoryRoot) {
  const cacheParent = join(repositoryRoot, 'artifacts', 'agent-runtime');
  const cacheDirectory = join(cacheParent, `vc-runtime-${validated.version}-${validated.package.sha256.slice(0, 12)}`);
  const marker = {
    schemaVersion: 1,
    version: validated.version,
    packageBytes: validated.package.bytes,
    packageSha256: validated.package.sha256,
    files: Object.fromEntries(validated.files.map(file => [file.name, file.sha256])),
  };
  const markerText = `${JSON.stringify(marker, null, 2)}\n`;
  const markerPath = join(cacheDirectory, 'manifest.json');
  const verifyCache = () => {
    const markerInfo = lstatSync(markerPath);
    if (!markerInfo.isFile() || markerInfo.isSymbolicLink() || readFileSync(markerPath, 'utf8') !== markerText) {
      fail(`existing VC Runtime cache is unowned or changed; preserved without overwrite: ${cacheDirectory}`);
    }
    const expectedEntries = [...WINDOWS_VC_RUNTIME_FILES, 'manifest.json'].sort();
    const actualEntries = readdirSync(cacheDirectory).sort();
    if (JSON.stringify(actualEntries) !== JSON.stringify(expectedEntries)) {
      fail(`existing VC Runtime cache has unexpected entries; preserved without overwrite: ${cacheDirectory}`);
    }
    for (const file of validated.files) {
      const cachedPath = join(cacheDirectory, file.name);
      const info = lstatSync(cachedPath);
      if (!info.isFile() || info.isSymbolicLink() || info.size !== file.bytes ||
          sha256(readFileSync(cachedPath)) !== file.sha256.toLowerCase()) {
        fail(`existing VC Runtime cache file changed; preserved without overwrite: ${file.name}`);
      }
    }
  };

  mkdirSync(cacheParent, { recursive: true });
  const parentInfo = lstatSync(cacheParent);
  if (!parentInfo.isDirectory() || parentInfo.isSymbolicLink()) fail(`VC Runtime cache path is not a regular directory: ${cacheParent}`);
  if (existsSync(cacheDirectory)) {
    const info = lstatSync(cacheDirectory);
    if (!info.isDirectory() || info.isSymbolicLink()) fail(`existing VC Runtime cache is not a regular directory: ${cacheDirectory}`);
    verifyCache();
  } else {
    const stage = mkdtempSync(join(cacheParent, '.vc-runtime-stage-'));
    try {
      for (const file of validated.files) copyFileSync(file.path, join(stage, file.name), fsConstants.COPYFILE_EXCL);
      writeFileSync(join(stage, 'manifest.json'), markerText, { flag: 'wx' });
      for (const file of validated.files) {
        const stagedPath = join(stage, file.name);
        if (sha256(readFileSync(stagedPath)) !== file.sha256.toLowerCase()) fail(`staged VC Runtime file changed during copy: ${file.name}`);
      }
      try {
        renameSync(stage, cacheDirectory);
      } catch (error) {
        if (!existsSync(cacheDirectory)) throw error;
        const info = lstatSync(cacheDirectory);
        if (!info.isDirectory() || info.isSymbolicLink()) throw error;
        verifyCache();
        rmSync(stage, { recursive: true, force: true });
      }
    } catch (error) {
      if (existsSync(stage)) rmSync(stage, { recursive: true, force: true });
      fail(`could not stage verified VC Runtime files: ${error.message}`);
    }
  }
  return {
    directory: cacheDirectory,
    ...validated,
    files: validated.files.map(file => ({ ...file, sourcePath: file.path, path: join(cacheDirectory, file.name) })),
  };
}

function findRuntimeDirectory() {
  const explicit = process.env.TEACELL_VC_RUNTIME_DIR;
  if (explicit) {
    const directory = resolve(explicit);
    if (!existsSync(directory)) fail(`TEACELL_VC_RUNTIME_DIR does not exist: ${directory}`);
    return directory;
  }

  const candidates = [];
  const addRoot = (root) => {
    if (!root || !existsSync(root)) return;
    if (root.toLowerCase().endsWith('microsoft.vc143.crt')) {
      candidates.push(resolve(root));
      return;
    }
    const direct = join(root, 'x64', 'Microsoft.VC143.CRT');
    if (existsSync(direct)) candidates.push(resolve(direct));
    const entries = readdirSync(root, { withFileTypes: true })
      .filter(entry => entry.isDirectory() && /^\d+(?:\.\d+){2,3}$/u.test(entry.name))
      .map(entry => join(root, entry.name, 'x64', 'Microsoft.VC143.CRT'))
      .filter(existsSync)
      .sort((left, right) => right.localeCompare(left, undefined, { numeric: true }));
    candidates.push(...entries.map(path => resolve(path)));
  };

  if (process.env.VCToolsRedistDir) addRoot(process.env.VCToolsRedistDir);
  const programFilesX86 = process.env['ProgramFiles(x86)'] ?? 'C:\\Program Files (x86)';
  const vswhere = join(programFilesX86, 'Microsoft Visual Studio', 'Installer', 'vswhere.exe');
  if (existsSync(vswhere)) {
    try {
      const installation = execFileSync(vswhere, [
        '-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath',
      ], { encoding: 'utf8', windowsHide: true }).trim();
      if (installation) addRoot(join(installation, 'VC', 'Redist', 'MSVC'));
    } catch (error) {
      fail(`vswhere could not locate the installed MSVC toolchain: ${error.message}`);
    }
  }
  const unique = [...new Set(candidates)];
  const candidate = unique.find(directory => WINDOWS_VC_RUNTIME_FILES.every(name => existsSync(join(directory, name))));
  if (!candidate) fail('signed x64 VC143 runtime files were not found; install the Visual C++ Redistributable component with Visual Studio Build Tools or set TEACELL_VC_RUNTIME_DIR');
  return candidate;
}

export function loadWindowsVCRuntime() {
  if (process.platform !== 'win32' || process.arch !== 'x64') {
    fail('the x64 Windows VC Runtime must be prepared by a native Windows x64 build');
  }
  const directory = findRuntimeDirectory();
  const script = [
    "$ErrorActionPreference = 'Stop'",
    '$root = $env:TEACELL_VC_RUNTIME_DIR',
    "$packagePath = Join-Path (Split-Path (Split-Path $root -Parent) -Parent) 'vc_redist.x64.exe'",
    '$package = Get-Item -LiteralPath $packagePath',
    '$packageSignature = Get-AuthenticodeSignature -LiteralPath $packagePath',
    "$names = @('" + WINDOWS_VC_RUNTIME_FILES.join("','") + "')",
    '$files = @(foreach ($name in $names) {',
    '  $path = Join-Path $root $name',
    '  $item = Get-Item -LiteralPath $path',
    '  $signature = Get-AuthenticodeSignature -LiteralPath $path',
    '  [pscustomobject]@{ name = $name; path = $item.FullName; version = $item.VersionInfo.FileVersion; bytes = $item.Length; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant(); signatureStatus = $signature.Status.ToString(); signer = $signature.SignerCertificate.Subject }',
    '})',
    '[pscustomobject]@{ schemaVersion = 1; version = (Get-Item -LiteralPath (Join-Path $root "msvcp140.dll")).VersionInfo.FileVersion; package = [pscustomobject]@{ version = $package.VersionInfo.FileVersion; bytes = $package.Length; sha256 = (Get-FileHash -LiteralPath $packagePath -Algorithm SHA256).Hash.ToLowerInvariant(); signatureStatus = $packageSignature.Status.ToString(); signer = $packageSignature.SignerCertificate.Subject }; files = $files } | ConvertTo-Json -Depth 5 -Compress',
  ].join('\n');
  let inventory;
  try {
    const stdout = execFileSync('pwsh.exe', ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', script], {
      encoding: 'utf8',
      windowsHide: true,
      env: { ...process.env, TEACELL_VC_RUNTIME_DIR: directory },
      maxBuffer: 1024 * 1024,
    });
    inventory = JSON.parse(stdout.trim());
  } catch (error) {
    const detail = error.message.split(/[\r\n]/u).slice(0, 2).join(' ').slice(0, 240);
    fail(`Microsoft VC Runtime signature inspection failed: ${detail}`);
  }
  const validated = validateVCRuntimeInventory(inventory);
  const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const staged = stageVCRuntimeInventory(validated, repositoryRoot);
  return {
    ...staged,
    packageBytes: inventory.package.bytes,
    packageSha256: inventory.package.sha256,
  };
}
