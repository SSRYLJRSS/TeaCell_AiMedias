import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import test from "node:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative, sep } from "node:path";
import {
  REQUIRED_PACKAGE_EXTENSIONS,
  RUNNER_BY_TARGET,
} from "./candidate-contract.mjs";
import { heifCandidateProvenance } from "./heif-contract.mjs";
import { SUPPORTED_CANDIDATE_TARGETS, verifyCandidateArtifacts } from "./verify-candidates.mjs";

const digest = (value) => createHash("sha256").update(value).digest("hex");
const version = "9.8.7";
const commit = "a".repeat(40);
const heifLicense = "fixture HEIF license\n";

function createFixture() {
  const parent = mkdtempSync(join(tmpdir(), "teacell-candidate-set-"));
  const artifactRoot = join(parent, "artifacts");
  mkdirSync(artifactRoot);
  const heifManifest = {
    schemaVersion: 1,
    release: { version: "26.7.0", releaseUrl: "https://example.invalid/heif" },
    libraries: [{
      name: "libheif",
      version: "1.23.1",
      license: "LGPL-3.0-or-later",
      repositoryUrl: "https://example.invalid/libheif",
      sourceCommit: "b".repeat(40),
      sourceUrl: "https://example.invalid/source",
      licenseUrl: "https://example.invalid/COPYING",
      licenseFileName: "libheif-COPYING",
      licenseSha256: digest(heifLicense),
    }],
    targets: Object.fromEntries(SUPPORTED_CANDIDATE_TARGETS.map((target) => {
      const staticLibraries = target === "x86_64-pc-windows-msvc"
        ? ["heif.lib", "x265-static.lib", "libde265.lib"]
        : ["libheif.a", "libx265.a", "libde265.a"];
      return [target, {
        runner: RUNNER_BY_TARGET[target],
        assetName: `${target}.zip`,
        assetUrl: `https://example.invalid/${target}.zip`,
        assetSha256: digest(target),
        assetSizeBytes: 1234,
        staticLibraries,
        staticLibrarySha256s: Object.fromEntries(staticLibraries.map((name) => [
          name,
          digest(`fixture HEIF archive member ${target}/${name}`),
        ])),
      }];
    })),
  };
  const mediaManifest = {
    targets: Object.fromEntries(SUPPORTED_CANDIDATE_TARGETS.map((target) => {
      const licenseFileName = `${target}-media-license.txt`;
      const license = `fixture media license for ${target}\n`;
      return [target, { licenseFileName, licenseSha256: digest(license), license }];
    })),
  };

  for (const target of SUPPORTED_CANDIDATE_TARGETS) {
    const dir = join(artifactRoot, `teacell-${target}`);
    const packagesDir = join(dir, "packages");
    const mediaLicenseDir = join(dir, "licenses", target);
    const heifLicenseDir = join(dir, "licenses", "heif");
    mkdirSync(packagesDir, { recursive: true });
    mkdirSync(mediaLicenseDir, { recursive: true });
    mkdirSync(heifLicenseDir, { recursive: true });

    const packageNames = REQUIRED_PACKAGE_EXTENSIONS[target].map((extension, index) => `fixture-${index}${extension}`)
      .sort((left, right) => left < right ? -1 : left > right ? 1 : 0);
    for (const name of packageNames) writeFileSync(join(packagesDir, name), `installer ${target} ${name}\n`);
    const media = mediaManifest.targets[target];
    writeFileSync(join(mediaLicenseDir, media.licenseFileName), media.license);
    writeFileSync(join(heifLicenseDir, "libheif-COPYING"), heifLicense);
    const buildLog = `native installer build log for ${target}\n`;
    writeFileSync(join(dir, "build.log"), buildLog);

    const heif = heifCandidateProvenance(heifManifest, target);
    const provenancePath = join(dir, "heif-provenance.json");
    writeFileSync(provenancePath, `${JSON.stringify(heif, null, 2)}\n`);
    const packageMetadata = packageNames.map((name) => {
      const bytes = readFileSync(join(packagesDir, name));
      return { name, sizeBytes: bytes.length, sha256: digest(bytes) };
    });
    const copiedFiles = [
      ...packageNames.map((name) => join(packagesDir, name)),
      join(mediaLicenseDir, media.licenseFileName),
      join(heifLicenseDir, "libheif-COPYING"),
      join(dir, "build.log"),
      provenancePath,
    ];
    const checksumRows = copiedFiles.map((file) => `${digest(readFileSync(file))}  ${relative(dir, file).split(sep).join("/")}`).sort();
    writeFileSync(join(dir, "build-manifest.json"), `${JSON.stringify({
      schemaVersion: 1,
      target,
      version,
      commit,
      classification: "unverified-manual-test-candidate",
      runner: RUNNER_BY_TARGET[target],
      packages: packageNames,
      packageMetadata,
      buildLog: { name: "build.log", sizeBytes: Buffer.byteLength(buildLog), sha256: digest(buildLog) },
      mediaToolsLicenseSha256: media.licenseSha256,
      heif,
    }, null, 2)}\n`);
    writeFileSync(join(dir, "SHA256SUMS.txt"), `${checksumRows.join("\n")}\n`);
  }

  return { parent, artifactRoot, packageVersion: version, expectedCommit: commit, mediaManifest, heifManifest };
}

test("candidate verifier accepts a complete three-target set from one version and commit", (context) => {
  const fixture = createFixture();
  context.after(() => rmSync(fixture.parent, { recursive: true, force: true }));

  const verified = verifyCandidateArtifacts(fixture);
  assert.deepEqual(verified.map(({ target }) => target), SUPPORTED_CANDIDATE_TARGETS);
  assert.ok(verified.every((row) => row.version === version && row.commit === commit));
});

test("candidate verifier rejects a package changed after checksum collection", (context) => {
  const fixture = createFixture();
  context.after(() => rmSync(fixture.parent, { recursive: true, force: true }));
  const packagePath = join(fixture.artifactRoot, "teacell-x86_64-pc-windows-msvc", "packages", "fixture-0.exe");
  writeFileSync(packagePath, "tampered installer\n");

  assert.throws(() => verifyCandidateArtifacts(fixture), /候选文件摘要不匹配/);
});

test("candidate verifier rejects a build log changed after checksum collection", (context) => {
  const fixture = createFixture();
  context.after(() => rmSync(fixture.parent, { recursive: true, force: true }));
  const logPath = join(fixture.artifactRoot, "teacell-x86_64-pc-windows-msvc", "build.log");
  writeFileSync(logPath, "tampered build log\n");

  assert.throws(() => verifyCandidateArtifacts(fixture), /构建日志缺失、大小或 SHA256 不匹配/);
});

test("candidate verifier rejects a target manifest from a different commit", (context) => {
  const fixture = createFixture();
  context.after(() => rmSync(fixture.parent, { recursive: true, force: true }));
  const manifestPath = join(fixture.artifactRoot, "teacell-aarch64-apple-darwin", "build-manifest.json");
  const build = JSON.parse(readFileSync(manifestPath, "utf8"));
  build.commit = "c".repeat(40);
  writeFileSync(manifestPath, `${JSON.stringify(build, null, 2)}\n`);

  assert.throws(() => verifyCandidateArtifacts(fixture), /目标、版本或提交不一致/);
});

test("candidate verifier rejects checksum paths that escape the artifact directory", (context) => {
  const fixture = createFixture();
  context.after(() => rmSync(fixture.parent, { recursive: true, force: true }));
  const checksumPath = join(fixture.artifactRoot, "teacell-x86_64-pc-windows-msvc", "SHA256SUMS.txt");
  const checksums = readFileSync(checksumPath, "utf8");
  writeFileSync(checksumPath, `${checksums}${"d".repeat(64)}  ../../outside.txt\n`);

  assert.throws(() => verifyCandidateArtifacts(fixture), /校验路径越界/);
});
