#!/usr/bin/env node
/* global process, console */
/** Verify that manually built platform artifacts share one version/commit and have intact checksums. */
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import {
  assertRunnerMatchesTarget,
  comparePackageNames,
  hasRequiredPackageExtensions,
  packageMetadataMatches,
} from "./candidate-contract.mjs";
import { heifCandidateProvenance, heifProvenanceMatches } from "./heif-contract.mjs";

export const SUPPORTED_CANDIDATE_TARGETS = Object.freeze([
  "x86_64-pc-windows-msvc",
  "aarch64-apple-darwin",
  "x86_64-unknown-linux-gnu",
]);

const digest = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

function filesUnder(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? filesUnder(path) : entry.isFile() ? [path] : [];
  });
}

/** Verify the complete three-target artifact set against the workflow's pinned inputs. */
export function verifyCandidateArtifacts({
  artifactRoot,
  expectedCommit,
  packageVersion,
  mediaManifest,
  heifManifest,
}) {
  const normalizedCommit = (expectedCommit ?? "").toLowerCase();
  if (!/^[a-f0-9]{40,64}$/.test(normalizedCommit)) {
    throw new Error("需要提供本次候选构建的完整 Git commit SHA");
  }
  if (!artifactRoot || !packageVersion || !mediaManifest || !heifManifest) {
    throw new Error("候选验证缺少 artifact 路径、版本或依赖 manifest");
  }

  const root = resolve(artifactRoot);
  const verified = [];
  for (const target of SUPPORTED_CANDIDATE_TARGETS) {
    const dir = join(root, `teacell-${target}`);
    const manifestPath = join(dir, "build-manifest.json");
    const checksumPath = join(dir, "SHA256SUMS.txt");
    if (!existsSync(manifestPath) || !existsSync(checksumPath)) throw new Error(`候选材料缺失：${target}`);
    const build = JSON.parse(readFileSync(manifestPath, "utf8"));
    if (build.target !== target || build.version !== packageVersion || build.commit !== normalizedCommit) {
      throw new Error(`目标、版本或提交不一致：${target}`);
    }
    assertRunnerMatchesTarget(target, build.runner);
    if (build.classification !== "unverified-manual-test-candidate") throw new Error(`候选包分类无效：${target}`);
    const buildLogPath = join(dir, "build.log");
    if (
      build.buildLog?.name !== "build.log" ||
      !existsSync(buildLogPath) ||
      !statSync(buildLogPath).isFile() ||
      build.buildLog.sizeBytes !== statSync(buildLogPath).size ||
      build.buildLog.sha256 !== digest(buildLogPath)
    ) throw new Error(`候选构建日志缺失、大小或 SHA256 不匹配：${target}`);

    const expectedHeif = heifCandidateProvenance(heifManifest, target);
    if (!heifProvenanceMatches(expectedHeif, build.heif)) throw new Error(`HEIF 来源、版本或许可证来源不一致：${target}`);
    const heifProvenancePath = join(dir, "heif-provenance.json");
    if (!existsSync(heifProvenancePath) || !heifProvenanceMatches(expectedHeif, JSON.parse(readFileSync(heifProvenancePath, "utf8")))) {
      throw new Error(`HEIF provenance 文件缺失或不匹配：${target}`);
    }
    for (const library of heifManifest.libraries) {
      const heifLicense = join(dir, "licenses", "heif", library.licenseFileName);
      if (!existsSync(heifLicense) || !statSync(heifLicense).isFile() || digest(heifLicense) !== library.licenseSha256) {
        throw new Error(`HEIF 许可证材料缺失或摘要不匹配：${target}/${library.licenseFileName}`);
      }
    }

    const licenseName = mediaManifest.targets?.[target]?.licenseFileName ?? "LICENSE.txt";
    const license = join(dir, "licenses", target, licenseName);
    const expectedLicenseHash = mediaManifest.targets?.[target]?.licenseSha256;
    if (build.mediaToolsLicenseSha256 !== expectedLicenseHash || !existsSync(license) || digest(license) !== expectedLicenseHash) {
      throw new Error(`许可证材料缺失或摘要不匹配：${target}`);
    }

    const checksums = new Map();
    for (const row of readFileSync(checksumPath, "utf8").split(/\r?\n/).filter(Boolean)) {
      const match = row.match(/^([a-f0-9]{64}) {2}(.+)$/i);
      if (!match || checksums.has(match[2])) throw new Error(`校验清单格式无效或重复：${target}`);
      const file = resolve(dir, match[2]);
      if (!file.startsWith(`${resolve(dir)}${sep}`)) throw new Error(`校验路径越界：${target}/${match[2]}`);
      if (!existsSync(file) || !statSync(file).isFile() || digest(file) !== match[1]) {
        throw new Error(`候选文件摘要不匹配：${target}/${match[2]}`);
      }
      checksums.set(match[2].split(sep).join("/"), match[1].toLowerCase());
    }
    if (checksums.size === 0) throw new Error(`校验清单为空：${target}`);

    const candidateFiles = filesUnder(dir);
    const actual = candidateFiles
      .filter((file) => file !== manifestPath && file !== checksumPath)
      .map((file) => relative(dir, file).split(sep).join("/"))
      .sort();
    const recorded = [...checksums.keys()].sort();
    if (actual.length !== recorded.length || actual.some((file, index) => file !== recorded[index])) {
      throw new Error(`校验清单未覆盖候选文件：${target}`);
    }
    const packageRows = candidateFiles
      .filter((file) => relative(dir, file).split(sep).join("/").startsWith("packages/"))
      .map((file) => {
        const relativeName = relative(join(dir, "packages"), file).split(sep).join("/");
        if (relativeName.includes("/")) throw new Error(`安装包目录结构无效：${target}/${relativeName}`);
        return { name: relativeName, sizeBytes: statSync(file).size, sha256: digest(file) };
      })
      .sort((a, b) => comparePackageNames(a.name, b.name));
    const packageNames = packageRows.map((entry) => entry.name);
    if (!build.packages?.length || JSON.stringify(build.packages) !== JSON.stringify(packageNames)) {
      throw new Error(`安装包清单与实际文件不一致：${target}`);
    }
    if (!hasRequiredPackageExtensions(target, packageNames)) {
      throw new Error(`计划要求的安装包类型不完整：${target}`);
    }
    if (!packageMetadataMatches(build.packageMetadata, packageRows)) {
      throw new Error(`安装包大小或 SHA256 与 manifest 不一致：${target}`);
    }
    verified.push({ target, version: build.version, commit: build.commit });
  }
  return verified;
}

function main() {
  const scriptRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const artifactRoot = resolve(process.argv[2] ?? join(scriptRoot, "artifacts"));
  const expectedCommit = process.argv[3] ?? process.env.GITHUB_SHA ?? "";
  const packageVersion = JSON.parse(readFileSync(join(scriptRoot, "package.json"), "utf8")).version;
  const mediaManifest = JSON.parse(readFileSync(join(scriptRoot, "src-tauri", "binaries", "manifest.json"), "utf8"));
  const heifManifest = JSON.parse(readFileSync(join(scriptRoot, "src-tauri", "native", "heif-manifest.json"), "utf8"));
  const verified = verifyCandidateArtifacts({ artifactRoot, expectedCommit, packageVersion, mediaManifest, heifManifest });
  for (const item of verified) console.log(`verified: ${item.target} ${item.version} ${item.commit}`);
  console.log("三个目标的候选包版本、Git 提交和摘要一致。此校验不替代真机 UAT、签名/公证或媒体工具再分发审查；不要据此公开分发。");
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
