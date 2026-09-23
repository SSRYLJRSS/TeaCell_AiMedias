#!/usr/bin/env node
/** 下载并校验三端 ffmpeg/ffprobe sidecar；所有来源和 hash 必须来自 manifest.json。 */
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binDir = join(root, "src-tauri", "binaries");
const manifest = JSON.parse(readFileSync(join(binDir, "manifest.json"), "utf8"));
const targetArg = process.argv.indexOf("--target");
const target = targetArg >= 0 ? process.argv[targetArg + 1] : null;
const meta = target && manifest.targets[target];
if (!meta) {
  console.error(`请指定 manifest 中的 target，例如 --target x86_64-pc-windows-msvc`);
  process.exit(1);
}

const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const isSafeRelative = (value) => {
  const normalized = value.replaceAll("\\", "/");
  return normalized && !normalized.startsWith("/") && !normalized.split("/").includes("..") && !normalized.includes(":");
};
const run = (command, args) => {
  const result = spawnSync(command, args, { stdio: "inherit" });
  if (result.error || result.status !== 0) {
    throw new Error(`${command} 失败: ${result.error?.message ?? result.status ?? "signal"}`);
  }
};

const capture = (command, args) => {
  const result = spawnSync(command, args, { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 });
  if (result.error || result.status !== 0) {
    throw new Error(`${command} 失败: ${result.error?.message ?? result.status ?? "signal"}`);
  }
  return result.stdout ?? "";
};

async function download(url, expected) {
  if (!/^https:\/\//i.test(url) || /\/latest(?:\/|$)/i.test(url)) throw new Error(`URL 未锁定到不可变 HTTPS release: ${url}`);
  const response = await fetch(url, { signal: AbortSignal.timeout(300_000) });
  if (!response.ok) throw new Error(`下载失败 ${response.status}: ${url}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (hash(bytes) !== expected) throw new Error(`归档 SHA256 不匹配: ${url}`);
  return bytes;
}

function validateArchive(archive, kind) {
  const isWindowsZip = kind === "zip" && process.platform === "win32";
  const list = kind === "zip"
    ? (isWindowsZip ? capture("tar", ["-tf", archive]) : capture("unzip", ["-Z1", archive]))
    : capture("tar", ["-tf", archive]);
  const entries = list.split(/\r?\n/).filter(Boolean);
  if (entries.length === 0) throw new Error(`归档为空: ${archive}`);
  for (const entry of entries) {
    if (!isSafeRelative(entry)) throw new Error(`归档路径不安全: ${entry}`);
  }

  // 拒绝链接和特殊文件，避免解压时写出目标目录或覆盖其他路径。
  if (kind === "tar.xz" || isWindowsZip) {
    const details = capture("tar", ["-tvf", archive]);
    for (const line of details.split(/\r?\n/).filter(Boolean)) {
      if (line[0] !== "-" && line[0] !== "d") throw new Error(`归档包含非普通文件/目录: ${line}`);
    }
  }
}

function extract(archive, kind, destination) {
  mkdirSync(destination, { recursive: true });
  validateArchive(archive, kind);
  if (kind === "tar.xz") run("tar", ["-xJf", archive, "-C", destination]);
  else if (process.platform === "win32") run("tar", ["-xf", archive, "-C", destination]);
  else run("unzip", ["-q", archive, "-d", destination]);
}

function locate(rootDir, relative) {
  if (!isSafeRelative(relative)) throw new Error(`manifest 路径不安全: ${relative}`);
  const candidate = resolve(rootDir, relative);
  if (!candidate.startsWith(resolve(rootDir) + sep) && candidate !== resolve(rootDir)) throw new Error(`manifest 路径越界: ${relative}`);
  if (!existsSync(candidate) || !statSync(candidate).isFile()) {
    const matches = [];
    const visit = (dir) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const item = join(dir, entry.name);
        if (entry.isDirectory()) visit(item);
        else if (entry.isFile() && entry.name === relative.split("/").at(-1)) matches.push(item.slice(rootDir.length + 1));
      }
    };
    visit(rootDir);
    const hint = matches.length ? `；归档中的同名条目：${matches.slice(0, 8).join(", ")}` : "";
    throw new Error(`归档内缺少文件: ${relative}${hint}`);
  }
  return candidate;
}

async function main() {
  const temp = join(tmpdir(), `bagertea-media-tools-${process.pid}-${Date.now()}`);
  mkdirSync(temp, { recursive: true });
  try {
    const archiveFor = (name) => meta[`${name}ArchiveUrl`] ?? meta.archiveUrl;
    const shaFor = (name) => meta[`${name}ArchiveSha256`] ?? meta.archiveSha256;
    const kindFor = (name) => meta[`${name}ArchiveKind`] ?? meta.archiveKind;
    const pathFor = (name) => meta[`${name}PathInArchive`];
    const outExt = meta.exeExt ?? "";
    const archives = new Map();
    const extractedDirs = new Map();
    const licenseName = meta.licenseFileName ?? "LICENSE.txt";
    if (!isSafeRelative(licenseName) || licenseName.includes("/")) throw new Error(`许可证文件名不安全: ${licenseName}`);

    async function getArchive(name) {
      const url = archiveFor(name);
      const kind = kindFor(name);
      const expectedHash = shaFor(name);
      if (!url || !kind || !expectedHash) throw new Error(`manifest 缺少 ${name} 归档信息`);
      let archive = archives.get(url);
      if (!archive) {
        const extension = kind === "tar.xz" ? "tar.xz" : "zip";
        archive = join(temp, `archive-${archives.size}.${extension}`);
        writeFileSync(archive, await download(url, expectedHash));
        archives.set(url, archive);
      }
      return { archive, url, kind };
    }

    async function getExtracted(name) {
      const { archive, url, kind } = await getArchive(name);
      let extracted = extractedDirs.get(url);
      if (!extracted) {
        extracted = join(temp, `extract-${extractedDirs.size}`);
        extract(archive, kind, extracted);
        extractedDirs.set(url, extracted);
      }
      return extracted;
    }

    for (const name of ["ffmpeg", "ffprobe"]) {
      const output = join(binDir, `${name}-${target}${outExt}`);
      const expected = meta[`${name}Sha256`];
      if (!/^[a-f0-9]{64}$/i.test(expected ?? "")) throw new Error(`manifest 中 ${name} SHA256 无效`);
      if (existsSync(output) && hash(readFileSync(output)) === expected) {
        if (process.platform !== "win32") chmodSync(output, 0o755);
        continue;
      }
      const extracted = await getExtracted(name);
      const source = locate(extracted, pathFor(name));
      const bytes = readFileSync(source);
      if (hash(bytes) !== expected) throw new Error(`${name} 二进制 SHA256 不匹配`);
      writeFileSync(output, bytes, { mode: 0o755 });
      console.log(`prepared ${output}`);
      if (process.platform !== "win32") chmodSync(output, 0o755);
    }

    const licenseDir = join(binDir, "licenses", target);
    mkdirSync(licenseDir, { recursive: true });
    if (!/^[a-f0-9]{64}$/i.test(meta.licenseSha256 ?? "")) throw new Error("manifest 中许可证 SHA256 无效");
    const licenseOutput = join(licenseDir, licenseName);
    if (existsSync(licenseOutput) && hash(readFileSync(licenseOutput)) === meta.licenseSha256) {
      console.log(`media tools ready: ${target}`);
      return;
    }
    let licenseBytes;
    if (meta.licensePathInArchive) {
      const extracted = await getExtracted("ffmpeg");
      const license = locate(extracted, meta.licensePathInArchive);
      licenseBytes = readFileSync(license);
    } else if (meta.licenseUrl) {
      licenseBytes = await download(meta.licenseUrl, meta.licenseSha256);
    } else throw new Error("manifest 缺少许可证来源");
    if (hash(licenseBytes) !== meta.licenseSha256) throw new Error("许可证 SHA256 不匹配");
    writeFileSync(licenseOutput, licenseBytes);
    console.log(`media tools ready: ${target}`);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

main().catch((error) => { console.error(`[media-tools] ${error.message}`); process.exit(1); });
