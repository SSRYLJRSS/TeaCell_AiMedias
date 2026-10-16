import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import test from 'node:test';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { stageVCRuntimeInventory, WINDOWS_VC_RUNTIME_FILES, WINDOWS_VC_RUNTIME_PIN, validateVCRuntimeInventory } from './windows-vc-runtime.mjs';

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'windows-vc-runtime-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const contents = Buffer.alloc(1024, 0x2a);
  const digest = createHash('sha256').update(contents).digest('hex');
  const version = '14.44.35211.0';
  const inventory = {
    schemaVersion: 1,
    version,
    package: {
      version,
      bytes: 25_635_768,
      signatureStatus: 'Valid',
      signer: 'CN=Microsoft Corporation, O=Microsoft Corporation',
      sha256: digest,
    },
    files: WINDOWS_VC_RUNTIME_FILES.map(name => ({
      name,
      path: join(directory, name),
      version,
      bytes: 1024,
      signatureStatus: 'Valid',
      signer: 'CN=Microsoft Windows Software Compatibility Publisher, O=Microsoft Corporation',
      sha256: digest,
    })),
  };
  for (const file of inventory.files) writeFileSync(file.path, contents, { flag: 'wx' });
  return {
    inventory,
    pin: {
      version,
      packageSha256: digest,
      files: Object.fromEntries(inventory.files.map(file => [file.name, { bytes: file.bytes, sha256: digest }])),
    },
  };
}

test('VC Runtime inventory requires the complete, same-version Microsoft-signed x64 file set', (t) => {
  const { inventory, pin } = fixture(t);
  const result = validateVCRuntimeInventory(inventory, pin);
  assert.equal(result.files.length, WINDOWS_VC_RUNTIME_FILES.length);
  assert.equal(result.version, '14.44.35211.0');
});

test('VC Runtime inventory rejects missing libraries and mixed runtime versions', (t) => {
  const missing = fixture(t);
  missing.inventory.files.pop();
  assert.throws(() => validateVCRuntimeInventory(missing.inventory, missing.pin), /complete x64 CRT file set/);

  const mixed = fixture(t);
  mixed.inventory.files[0].version = '14.44.35210.0';
  assert.throws(() => validateVCRuntimeInventory(mixed.inventory, mixed.pin), /untrusted or inconsistent/);
});

test('VC Runtime inventory rejects invalid Microsoft signatures and package hash metadata', (t) => {
  const unsigned = fixture(t);
  unsigned.inventory.files[1].signatureStatus = 'NotSigned';
  assert.throws(() => validateVCRuntimeInventory(unsigned.inventory, unsigned.pin), /untrusted or inconsistent/);

  const untrusted = fixture(t);
  untrusted.inventory.files[2].signer = 'CN=Another Publisher, O=Untrusted';
  assert.throws(() => validateVCRuntimeInventory(untrusted.inventory, untrusted.pin), /untrusted or inconsistent/);

  const packageHashMissing = fixture(t);
  packageHashMissing.inventory.package.sha256 = 'invalid';
  assert.throws(() => validateVCRuntimeInventory(packageHashMissing.inventory, packageHashMissing.pin), /package signature, version, or SHA256/);
});

test('production VC Runtime pin rejects validly signed but unapproved servicing versions', (t) => {
  const { inventory } = fixture(t);
  inventory.version = '14.44.35212.0';
  inventory.package.version = inventory.version;
  for (const file of inventory.files) file.version = inventory.version;
  assert.throws(() => validateVCRuntimeInventory(inventory), /version or schema is invalid/);
  assert.equal(WINDOWS_VC_RUNTIME_PIN.version, '14.44.35211.0');
  assert.equal(Object.keys(WINDOWS_VC_RUNTIME_PIN.files).length, WINDOWS_VC_RUNTIME_FILES.length);
});

test('VC Runtime staging is content-addressed, idempotent, and preserves a changed cache', (t) => {
  const { inventory, pin } = fixture(t);
  const validated = validateVCRuntimeInventory(inventory, pin);
  const repository = mkdtempSync(join(tmpdir(), 'windows-vc-runtime-stage-'));
  t.after(() => rmSync(repository, { recursive: true, force: true }));

  const first = stageVCRuntimeInventory(validated, repository);
  assert.match(first.directory, /artifacts[\\/]agent-runtime[\\/]vc-runtime-14\.44\.35211\.0-/u);
  assert.deepEqual(readdirSync(first.directory).sort(), [...WINDOWS_VC_RUNTIME_FILES, 'manifest.json'].sort());
  const second = stageVCRuntimeInventory(validated, repository);
  assert.equal(second.directory, first.directory);

  const tamperedPath = join(first.directory, WINDOWS_VC_RUNTIME_FILES[0]);
  writeFileSync(tamperedPath, 'preserve this changed cache');
  assert.throws(() => stageVCRuntimeInventory(validated, repository), /cache file changed; preserved without overwrite/u);
  assert.equal(readFileSync(tamperedPath, 'utf8'), 'preserve this changed cache');
});
