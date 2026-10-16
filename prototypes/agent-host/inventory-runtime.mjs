// Local artifact inventory only. Declared package licenses are not a legal
// conclusion or proof that embedded/example assets have the same license.
import process from 'node:process';
import { readFileSync, readdirSync, existsSync, statSync, mkdirSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, resolve, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const prototype = dirname(fileURLToPath(import.meta.url));
const moduleRoot = join(prototype, 'node_modules');
const output = resolve(prototype, '../../artifacts/agent-runtime/dependency-inventory.json');
const lockPath = join(prototype, 'package-lock.json');
const lock = JSON.parse(readFileSync(lockPath, 'utf8'));
const hash = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const slash = path => path.replaceAll('\\', '/');
const packages = [];
const missingOptional = [];
for (const [location, record] of Object.entries(lock.packages)) {
  if (!location.startsWith('node_modules/')) continue;
  const full = resolve(prototype, location);
  if (!full.startsWith(moduleRoot + sep)) throw new Error('UNEXPECTED_LOCK_LOCATION');
  const manifestPath = join(full, 'package.json');
  if (!existsSync(manifestPath)) {
    if (!record.optional) throw new Error(`MISSING_REQUIRED_PACKAGE:${location}`);
    missingOptional.push({ location, version: record.version });
    continue;
  }
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  if (manifest.version !== record.version) throw new Error(`PACKAGE_VERSION_DRIFT:${location}`);
  const licenseFiles = readdirSync(full, { withFileTypes: true })
    .filter(entry => entry.isFile() && /^(licen[cs]e|copying|notice)([.-]|$)/i.test(entry.name))
    .map(entry => ({ name: entry.name, bytes: statSync(join(full, entry.name)).size, sha256: hash(join(full, entry.name)) }));
  packages.push({ location, name: manifest.name, version: manifest.version,
    manifestSha256: hash(manifestPath), declaredLicense: manifest.license ?? null,
    integrity: record.integrity ?? null, optional: record.optional ?? false, licenseFiles });
}
let bytes = 0; let files = 0;
const assets = []; const links = [];
function walk(directory) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isSymbolicLink()) { links.push(slash(relative(moduleRoot, path))); continue; }
    if (entry.isDirectory()) { walk(path); continue; }
    if (!entry.isFile()) continue;
    const size = statSync(path).size;
    bytes += size; files++;
    if (/\.(node|wasm|dll|exe)$/i.test(entry.name)) {
      const location = slash(relative(prototype, path));
      const owner = packages.filter(pkg => location.startsWith(`${pkg.location}/`))
        .sort((left, right) => right.location.length - left.location.length)[0];
      assets.push({ path: slash(relative(moduleRoot, path)), bytes: size, sha256: hash(path),
        package: owner?.name ?? null, packageVersion: owner?.version ?? null,
        declaredPackageLicense: owner?.declaredLicense ?? null,
        exampleAsset: /\/examples\//.test(location) });
    }
  }
}
walk(moduleRoot);
const report = { schema: 1, recordedAt: new Date().toISOString(), platform: process.platform, arch: process.arch,
  lockSha256: hash(lockPath), totalFiles: files, totalBytes: bytes, packages, missingOptional, assets, links,
  packagesWithoutRootLicenseText: packages.filter(pkg => pkg.licenseFiles.length === 0).map(pkg => pkg.location),
  limitations: ['Local installed dependencies only; absent optional platform packages are listed separately',
    'Package metadata license is not proof of embedded asset license',
    'Example and foreign platform assets are present in the unfiltered experiment',
    'Does not prove installer extraction, executable loading or redistribution compliance'] };
mkdirSync(dirname(output), { recursive: true });
writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
process.stdout.write(`${JSON.stringify({ output, packages: packages.length, assets: assets.length, bytes, files,
  absentOptionalPackages: missingOptional.length, packagesWithoutRootLicenseText: report.packagesWithoutRootLicenseText.length, links: links.length })}\n`);
