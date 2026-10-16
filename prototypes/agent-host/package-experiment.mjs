import process from 'node:process';
import { globSync, readFileSync, writeFileSync, mkdirSync, existsSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

// Generate only a temporary overlay; never edit production Tauri configuration.
const prototype = dirname(fileURLToPath(import.meta.url));
const root = resolve(prototype, '../..');
const tauri = join(root, 'src-tauri');
const output = join(root, 'artifacts/agent-runtime');
const manifest = JSON.parse(readFileSync(join(prototype, 'runtime-manifest.json'), 'utf8'));
const target = manifest.targets['x86_64-pc-windows-msvc'];
const runtime = resolve(output, dirname(target.executable));
const resources = {};
const packageLock = JSON.parse(readFileSync(join(prototype, 'package-lock.json'), 'utf8'));
const licenseManifest = JSON.parse(readFileSync(join(prototype, 'licenses/manifest.json'), 'utf8'));
if (licenseManifest.schema !== 1 || !licenseManifest.licenses?.some(license => license.id === 'pi-monorepo-1.0.4')) throw new Error('PI_LICENSE_RESOURCE_REQUIRED');
for (const license of licenseManifest.licenses) {
  if (!/^[a-zA-Z0-9_.-]+\.txt$/.test(license.path) || !/^[a-f0-9]{64}$/.test(license.sha256)) throw new Error('INVALID_LICENSE_RESOURCE');
  const contents = readFileSync(join(prototype, 'licenses', license.path));
  if (createHash('sha256').update(contents).digest('hex') !== license.sha256) throw new Error('LICENSE_DIGEST_MISMATCH');
  for (const pkg of license.packages) {
    if (!Object.entries(packageLock.packages).some(([location, record]) =>
      (location === `node_modules/${pkg.name}` || location.endsWith(`/node_modules/${pkg.name}`)) && record.version === pkg.version)) throw new Error('LICENSE_PACKAGE_VERSION_MISMATCH');
  }
}
const slash = value => value.replaceAll('\\', '/');
function source(path) {
  const full = resolve(path);
  if (!full.startsWith(root + sep) || !existsSync(full)) throw new Error('PACKAGE_SOURCE_UNAVAILABLE');
  return slash(full);
}

const sidecar = JSON.parse(readFileSync(join(tauri, 'tauri.sidecar.conf.json'), 'utf8'));
for (const pattern of sidecar.bundle.resources) {
  for (const path of globSync(pattern, { cwd: tauri })) {
    const full = resolve(tauri, path);
    if (statSync(full).isFile()) {
      resources[source(full)] = slash(relative(tauri, full)).replaceAll('../', '_up_/');
    }
  }
}
resources[source(join(prototype, 'node_modules')) + '/'] = 'agent-poc/node_modules/';
resources[source(runtime) + '/'] = 'agent-poc/runtime/';
for (const name of globSync('*.mjs', { cwd: prototype })) {
  resources[source(join(prototype, name))] = `agent-poc/${name}`;
}
if (!/^[a-zA-Z0-9_.-]+\.txt$/.test(manifest.checksumFile) || createHash('sha256').update(readFileSync(join(prototype, manifest.checksumFile))).digest('hex') !== manifest.checksumFileSha256) throw new Error('RUNTIME_CHECKSUM_SOURCE_MISMATCH');
for (const name of ['package.json', 'package-lock.json', 'runtime-manifest.json', manifest.checksumFile]) {
  resources[source(join(prototype, name))] = `agent-poc/${name}`;
}
for (const name of globSync('licenses/*', { cwd: prototype })) {
  resources[source(join(prototype, name))] = `agent-poc/${slash(name)}`;
}
resources[source(join(prototype, 'native-owner/target/debug/teacell-agent-owner-poc.exe'))] = 'agent-poc/native-owner.exe';
mkdirSync(output, { recursive: true });
const config = join(output, 'bundle-poc.conf.json');
writeFileSync(config, JSON.stringify({ bundle: { resources } }, null, 2) + '\n');
process.stdout.write(config + '\n');
