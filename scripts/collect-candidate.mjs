#!/usr/bin/env node
/* global process, console */
/** Collect native Tauri installers for one target; this never publishes or signs them. */
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, isAbsolute, join, relative, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";
import {
  assertRunnerMatchesTarget,
  assertCandidateCheckout,
  comparePackageNames,
  filterPackageFilesByVersion,
  hasRequiredPackageExtensions,
} from "./candidate-contract.mjs";
import { heifCandidateProvenance, verifyHeifCache } from "./heif-contract.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const targets = {
  "x86_64-pc-windows-msvc": [".exe", ".msi"],
  "aarch64-apple-darwin": [".dmg"],
  "x86_64-unknown-linux-gnu": [".appimage", ".deb"],
};
const args = process.argv.slice(2);
const valueAfter = (name) => {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : undefined;
};
const target = valueAfter("--target");
const commit = valueAfter("--commit");
const out = valueAfter("--out");
if (!targets[target] || !/^[a-f0-9]{40,64}$/i.test(commit ?? "") || !out || isAbsolute(out)) {
  throw new Error("用法：node scripts/collect-candidate.mjs --target <triple> --commit <sha> --out <directory>");
}

function gitOutput(args) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) {
    throw new Error(`无法验证候选源码 Git 状态：${result.error?.message ?? result.stderr ?? result.status ?? "signal"}`);
  }
  return result.stdout ?? "";
}

assertCandidateCheckout(
  commit,
  gitOutput(["rev-parse", "HEAD"]).trim(),
  gitOutput(["status", "--porcelain=v1", "--untracked-files=all"]),
);
const runner = { os: process.platform, arch: process.arch };
assertRunnerMatchesTarget(target, runner);

const packageJson = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const mediaManifest = JSON.parse(readFileSync(join(root, "src-tauri", "binaries", "manifest.json"), "utf8"));
const heifManifest = JSON.parse(readFileSync(join(root, "src-tauri", "native", "heif-manifest.json"), "utf8"));
const heifCache = join(root, "src-tauri", "native", "heif", target);
const heifCheck = verifyHeifCache(heifCache, heifManifest, target);
if (!heifCheck.ok) throw new Error(`候选包 HEIF 原生依赖未通过来源/许可证校验：${heifCheck.reason}`);
const heifProvenance = heifCandidateProvenance(heifManifest, target);
const bundleDir = join(root, "src-tauri", "target", target, "release", "bundle");
const outDir = resolve(root, out);
if (!outDir.startsWith(`${root}${sep}`)) throw new Error("候選輸出目錄必須位於 repository 子目錄內");
if (!existsSync(bundleDir)) throw new Error(`Tauri bundle 目录不存在：${bundleDir}`);
if (existsSync(outDir) && readdirSync(outDir).length > 0) throw new Error(`输出目录必须不存在或为空：${outDir}`);

function filesUnder(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? filesUnder(path) : entry.isFile() ? [path] : [];
  });
}

const bundlePackages = filesUnder(bundleDir).filter((file) =>
  targets[target].includes(file.slice(file.lastIndexOf(".")).toLowerCase()),
);
const installers = filterPackageFilesByVersion(bundlePackages, packageJson.version);
if (installers.length === 0) throw new Error(`未找到 ${target} 安装包`);
const installerNames = installers.map((file) => basename(file));
if (!hasRequiredPackageExtensions(target, installerNames)) {
  throw new Error(`${target} 缺少计划要求的安装包类型：${targets[target].join(", ")}`);
}

const media = mediaManifest.targets?.[target];
const licenseName = media?.licenseFileName ?? "LICENSE.txt";
if (!/^[^\\/]+$/.test(licenseName)) throw new Error("媒体工具许可证文件名无效");
const licenseSource = join(root, "src-tauri", "binaries", "licenses", target, licenseName);
if (!existsSync(licenseSource) || !statSync(licenseSource).isFile()) throw new Error(`候选包缺少许可证材料：${licenseSource}`);
const buildLogSource = process.env.CANDIDATE_BUILD_LOG;
if (!buildLogSource || !existsSync(buildLogSource) || !statSync(buildLogSource).isFile() || statSync(buildLogSource).size === 0) {
  throw new Error("候选包缺少本次原生安装包构建日志；设置 CANDIDATE_BUILD_LOG 并重新构建");
}

mkdirSync(join(outDir, "packages"), { recursive: true });
mkdirSync(join(outDir, "licenses", target), { recursive: true });
mkdirSync(join(outDir, "licenses", "heif"), { recursive: true });
const copied = [];
const names = new Set();
for (const installer of installers) {
  const name = basename(installer);
  if (names.has(name)) throw new Error(`安装包文件名冲突：${name}`);
  names.add(name);
  const destination = join(outDir, "packages", name);
  copyFileSync(installer, destination);
  copied.push(destination);
}
const licenseDestination = join(outDir, "licenses", target, licenseName);
copyFileSync(licenseSource, licenseDestination);
copied.push(licenseDestination);
for (const library of heifManifest.libraries) {
  const source = join(heifCache, "licenses", library.licenseFileName);
  const destination = join(outDir, "licenses", "heif", library.licenseFileName);
  copyFileSync(source, destination);
  copied.push(destination);
}
const buildLogDestination = join(outDir, "build.log");
copyFileSync(buildLogSource, buildLogDestination);
copied.push(buildLogDestination);
const heifProvenancePath = join(outDir, "heif-provenance.json");
writeFileSync(heifProvenancePath, `${JSON.stringify(heifProvenance, null, 2)}\n`, { flag: "wx" });
copied.push(heifProvenancePath);

const digest = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const buildLog = {
  name: "build.log",
  sizeBytes: statSync(buildLogDestination).size,
  sha256: digest(buildLogDestination),
};
const packageMetadata = installers.map((installer) => {
  const name = basename(installer);
  const file = join(outDir, "packages", name);
  return { name, sizeBytes: statSync(file).size, sha256: digest(file) };
}).sort((a, b) => comparePackageNames(a.name, b.name));
const fileRows = copied.map((file) => `${digest(file)}  ${relative(outDir, file).split(sep).join("/")}`).sort();
const buildManifest = {
  schemaVersion: 1,
  target,
  version: packageJson.version,
  commit: commit.toLowerCase(),
  classification: "unverified-manual-test-candidate",
  runner,
  packages: packageMetadata.map((entry) => entry.name),
  packageMetadata,
  buildLog,
  mediaToolsLicenseSha256: media.licenseSha256,
  heif: heifProvenance,
};
writeFileSync(join(outDir, "build-manifest.json"), `${JSON.stringify(buildManifest, null, 2)}\n`, { flag: "wx" });
writeFileSync(join(outDir, "SHA256SUMS.txt"), `${fileRows.join("\n")}\n`, { flag: "wx" });
console.log(`candidate collected: ${target} ${packageJson.version} ${commit}`);
