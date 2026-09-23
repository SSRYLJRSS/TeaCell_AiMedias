#!/usr/bin/env node
/**
 * P0-02：三目标 FFmpeg/FFprobe sidecar 准备脚本。
 *
 * 职责（严格、可复现、可校验）：
 *  - 只接受 manifest.json 白名单里的 target；
 *  - 按 manifest 下载 ffmpeg/ffprobe 归档到临时目录（允许 `ffprobeArchive*` 分离来源），
 *    校验归档 SHA256（哨兵值一律拒绝，绝不下载未校验二进制）；
 *  - 解压时防 Zip Slip（拒绝逃出目标目录的成员）；
 *  - 取出 ffmpeg/ffprobe，重命名为 Tauri externalBin 规则 `<name>-<triple><ext>` 放入 src-tauri/binaries/；
 *  - Unix 目标设置可执行位；
 *  - 再次校验最终文件 SHA256；
 *  - 运行 `ffmpeg -version` / `ffprobe -version` 与 `ffmpeg -hide_banner -encoders`，
 *    断言 manifest.requiredEncoders 全部存在（Windows/macOS H.264/AAC，Linux VP8/WebM）；
 *  - 复制许可证文件（若 manifest 指定）到发布材料。
 *
 * 用法：
 *   node scripts/prepare-media-tools.mjs --target <triple> [--force]
 *
 * 设计边界：新增目标或替换来源若仍使用占位哨兵（REPLACE_WITH_REAL_SHA256），维护者填入真实值前，
 * 本脚本会在校验阶段明确失败——这是有意的发布闸，防止 CI 临时抓取未校验的 latest 产物。
 */

import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  mkdirSync,
  copyFileSync,
  chmodSync,
  createReadStream,
  lstatSync,
  readdirSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "..");
const srcTauri = join(repoRoot, "src-tauri");
const binDir = join(srcTauri, "binaries");
const manifestPath = join(binDir, "manifest.json");

const SENTINEL = "REPLACE_WITH_REAL_SHA256";
const URL_SENTINEL = "REPLACE_WITH_IMMUTABLE_RELEASE_URL";

function die(msg) {
  console.error(`\n[prepare-media-tools] 错误：${msg}\n`);
  process.exit(1);
}

function parseArgs(argv) {
  let target = null;
  let force = false;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--target" && argv[i + 1]) {
      target = argv[i + 1];
      i++;
    } else if (argv[i] === "--force") {
      force = true;
    }
  }
  return { target, force };
}

function sha256File(file) {
  return new Promise((res, rej) => {
    const h = createHash("sha256");
    const s = createReadStream(file);
    s.on("error", rej);
    s.on("data", (d) => h.update(d));
    s.on("end", () => res(h.digest("hex")));
  });
}

function sha256FileSync(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

function loadManifest() {
  if (!existsSync(manifestPath)) {
    die(`未找到 sidecar 清单：${manifestPath}`);
  }
  try {
    return JSON.parse(readFileSync(manifestPath, "utf8"));
  } catch (e) {
    die(`解析 manifest 失败：${e.message}`);
  }
}

/** 断言值不是占位哨兵；哨兵 = 维护者尚未填入真实校验值 → 拒绝执行。 */
function assertNotSentinel(value, label) {
  if (!value || value === SENTINEL) {
    die(
      `${label} 仍是占位值（${SENTINEL}）。发布前必须由维护者填入真实 SHA256，` +
        `脚本不会下载/放行未校验的二进制。`,
    );
  }
}

/** 拒绝可漂移的 latest URL：即使未来误填了 SHA，也不能把未锁定版本带进候选包。 */
function assertPinnedArchiveUrl(value, label) {
  if (
    typeof value !== "string" ||
    value === URL_SENTINEL ||
    !/^https?:\/\//i.test(value) ||
    /\/latest(?:\/|$)/i.test(value)
  ) {
    die(`${label} 未锁定到不可变版本 URL（禁止使用 latest）。请填入具体 release/tag 地址后再准备 sidecar。`);
  }
}

function run(cmd, args) {
  return spawnSync(cmd, args, { encoding: "utf8" });
}

/**
 * 列出归档成员并在解压前拒绝绝对路径、父目录跳转和 NUL。
 *
 * 解压到临时目录本身不是 Zip Slip 防护：恶意归档仍可能通过 `../` 写出临时目录，
 * 或用符号链接让后续的 basename 查找拿到临时目录外的文件。因此先审计成员名，
 * 解压后 `findFile` 也会跳过符号链接。
 */
function listArchiveMembers(archive, kind, label) {
  let listed;
  if (kind === "zip") {
    // Windows runner 自带 bsdtar；Unix runner 用 unzip 的纯成员列表模式。
    listed = process.platform === "win32"
      ? run("tar", ["-tf", archive])
      : run("unzip", ["-Z1", archive]);
  } else if (kind === "tar.xz") {
    listed = run("tar", ["-tJf", archive]);
  } else {
    die(`${label} 使用了未知归档类型：${kind}`);
  }
  if (listed.status !== 0) {
    die(`${label} 无法列出归档成员：${listed.stderr || ""}`);
  }
  return String(listed.stdout || "")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

function assertSafeArchiveMembers(members, label) {
  for (const member of members) {
    const normalized = member.replace(/\\/g, "/");
    const segments = normalized.split("/");
    if (
      normalized.includes("\0") ||
      normalized.startsWith("/") ||
      /^[A-Za-z]:\//.test(normalized) ||
      segments.some((segment) => segment === "..")
    ) {
      die(`${label} 含不安全归档成员路径：${member}`);
    }
  }
}

function extractArchive(archive, kind, extractDir, label) {
  const members = listArchiveMembers(archive, kind, label);
  assertSafeArchiveMembers(members, label);
  mkdirSync(extractDir, { recursive: true });
  if (kind === "zip") {
    const unzip = process.platform === "win32"
      ? run("tar", ["-xf", archive, "-C", extractDir])
      : run("unzip", ["-q", "-o", archive, "-d", extractDir]);
    if (unzip.status !== 0) die(`${label} 解压 zip 失败：${unzip.stderr || ""}`);
  } else {
    const untar = run("tar", ["-xJf", archive, "-C", extractDir]);
    if (untar.status !== 0) die(`${label} 解压 tar.xz 失败：${untar.stderr || ""}`);
  }
}

async function downloadAndExtract({ url, kind, expectedSha, label, work, extractDir }) {
  assertNotSentinel(expectedSha, `${label}.archiveSha256`);
  const archive = join(work, `${label.replace(/[^A-Za-z0-9_-]/g, "_")}.${kind}`);
  console.log(`[prepare-media-tools] 下载 ${label}：${url}`);
  const dl = run("curl", ["-fSL", "-o", archive, url]);
  if (dl.status !== 0) {
    die(`下载失败：${url}\n${dl.stderr || ""}`);
  }
  const gotArchive = await sha256File(archive);
  if (gotArchive !== expectedSha) {
    die(`${label} 归档 SHA256 不匹配：\n  期望 ${expectedSha}\n  实际 ${gotArchive}`);
  }
  console.log(`[prepare-media-tools] ${label} 归档 SHA256 校验通过。`);
  extractArchive(archive, kind, extractDir, label);
  return archive;
}

/** 对已存在或刚准备好的 sidecar 执行同一套运行时能力检查。 */
function validatePreparedTools(ffmpegPath, ffprobePath, meta, target) {
  const ver = run(ffmpegPath, ["-hide_banner", "-version"]);
  if (ver.status !== 0) {
    die(`${target} ffmpeg -version 失败：${ver.stderr || ""}`);
  }
  const probeVer = run(ffprobePath, ["-hide_banner", "-version"]);
  if (probeVer.status !== 0) {
    die(`${target} ffprobe -version 失败：${probeVer.stderr || ""}`);
  }
  console.log(`[prepare-media-tools] ${String(ver.stdout || "").split("\n")[0]}`);
  const enc = run(ffmpegPath, ["-hide_banner", "-encoders"]);
  if (enc.status !== 0) {
    die(`${target} ffmpeg -encoders 失败：${enc.stderr || ""}`);
  }
  const encoderText = `${enc.stdout || ""}\n${enc.stderr || ""}`;
  const missing = (meta.requiredEncoders || []).filter((e) => !encoderText.includes(e));
  if (missing.length > 0) {
    die(`${target} sidecar 缺少必需编码器：${missing.join(", ")}（该目标转码链路依赖它们）。`);
  }
  console.log(`[prepare-media-tools] 编码器齐全：${(meta.requiredEncoders || []).join(", ")}`);
}

async function copyLicenseMaterial(meta, target, extractDirs, binDir) {
  const licDir = join(binDir, "licenses", target);
  mkdirSync(licDir, { recursive: true });
  if (meta.licensePathInArchive) {
    const wantLic = basename(meta.licensePathInArchive);
    const foundLic = extractDirs.map((dir) => findFile(dir, wantLic)).find(Boolean);
    if (foundLic) {
      copyFileSync(foundLic, join(licDir, wantLic));
      console.log(`[prepare-media-tools] 许可证 : ${join("licenses", target, wantLic)}`);
      return;
    }
    if (!meta.licenseUrl) {
      die(`${target} 归档中未找到许可证 ${wantLic}，拒绝把无许可证材料的 sidecar 当作可发布产物。`);
    }
  }
  if (meta.licenseUrl) {
    assertPinnedArchiveUrl(meta.licenseUrl, `${target}.licenseUrl`);
    assertNotSentinel(meta.licenseSha256, `${target}.licenseSha256`);
    const fileName = meta.licenseFileName || "LICENSE.txt";
    const work = mkdtempSync(join(tmpdir(), "mediatools-license-"));
    const downloaded = join(work, fileName);
    const dl = run("curl", ["-fSL", "-o", downloaded, meta.licenseUrl]);
    if (dl.status !== 0) die(`许可证下载失败：${meta.licenseUrl}\n${dl.stderr || ""}`);
    return sha256File(downloaded).then((actual) => {
      if (actual !== meta.licenseSha256) {
        die(`${target} 许可证 SHA256 不匹配：\n  期望 ${meta.licenseSha256}\n  实际 ${actual}`);
      }
      copyFileSync(downloaded, join(licDir, fileName));
      console.log(`[prepare-media-tools] 许可证 : ${join("licenses", target, fileName)}`);
    });
  }
}

function assertLicenseMaterialPresent(meta, target, binDir) {
  if (!meta.licensePathInArchive && !meta.licenseUrl) return;
  const wantLic = meta.licenseFileName || (meta.licensePathInArchive && basename(meta.licensePathInArchive));
  const path = wantLic ? join(binDir, "licenses", target, wantLic) : null;
  if (!path || !existsSync(path) || !lstatSync(path).isFile()) {
    die(`${target} 已存在 sidecar，但许可证材料缺失：${path}`);
  }
  if (meta.licenseSha256 && meta.licenseSha256 !== SENTINEL) {
    const actual = sha256FileSync(path);
    if (actual !== meta.licenseSha256) {
      die(`${target} 已存在 sidecar，但许可证 SHA256 不匹配：\n  期望 ${meta.licenseSha256}\n  实际 ${actual}`);
    }
  } else if (meta.licenseSha256 === SENTINEL) {
    die(`${target}.licenseSha256 仍是占位值（${SENTINEL}）。`);
  }
}

async function main() {
  const { target, force } = parseArgs(process.argv.slice(2));
  const manifest = loadManifest();
  if (!target) {
    die(`必须用 --target 指定目标之一：\n  ${Object.keys(manifest.targets).join("\n  ")}`);
  }
  const meta = manifest.targets[target];
  if (!meta) {
    die(`不支持的 target：${target}。白名单：\n  ${Object.keys(manifest.targets).join("\n  ")}`);
  }

  console.log(`[prepare-media-tools] 目标 : ${target}（ffmpeg ${manifest.ffmpegVersion}）`);

  const ffmpegOut = join(binDir, `ffmpeg-${target}${meta.exeExt}`);
  const ffprobeOut = join(binDir, `ffprobe-${target}${meta.exeExt}`);

  // 来源 URL 即使在「已就绪跳过」路径也必须不可漂移，避免 latest + 旧文件被误当可复现产物。
  assertPinnedArchiveUrl(meta.archiveUrl, `${target}.archiveUrl`);
  const ffprobeArchiveUrl = meta.ffprobeArchiveUrl || meta.archiveUrl;
  const ffprobeArchiveKind = meta.ffprobeArchiveKind || meta.archiveKind;
  const ffprobeArchiveSha = meta.ffprobeArchiveSha256 || meta.archiveSha256;
  assertPinnedArchiveUrl(ffprobeArchiveUrl, `${target}.ffprobeArchiveUrl`);
  assertNotSentinel(meta.archiveSha256, `${target}.archiveSha256`);
  assertNotSentinel(ffprobeArchiveSha, `${target}.ffprobeArchiveSha256`);

  // 已存在且校验通过则跳过（除非 --force）。
  if (!force && existsSync(ffmpegOut) && existsSync(ffprobeOut)) {
    const okFf = (await sha256File(ffmpegOut)) === meta.ffmpegSha256;
    const okFp = (await sha256File(ffprobeOut)) === meta.ffprobeSha256;
    if (okFf && okFp && meta.ffmpegSha256 !== SENTINEL) {
      assertNotSentinel(meta.ffmpegSha256, `${target}.ffmpegSha256`);
      assertNotSentinel(meta.ffprobeSha256, `${target}.ffprobeSha256`);
      assertLicenseMaterialPresent(meta, target, binDir);
      validatePreparedTools(ffmpegOut, ffprobeOut, meta, target);
      console.log("[prepare-media-tools] sidecar 已就绪且 SHA256 匹配，跳过。用 --force 强制重装。");
      return;
    }
  }

  // 发布闸：最终文件 SHA256 必须是真实值，否则拒绝下载。
  assertNotSentinel(meta.ffmpegSha256, `${target}.ffmpegSha256`);
  assertNotSentinel(meta.ffprobeSha256, `${target}.ffprobeSha256`);

  mkdirSync(binDir, { recursive: true });
  const work = mkdtempSync(join(tmpdir(), "mediatools-"));
  console.log(`[prepare-media-tools] 工作目录 : ${work}`);

  const ffmpegExtractDir = join(work, "extract-ffmpeg");
  await downloadAndExtract({
    url: meta.archiveUrl,
    kind: meta.archiveKind,
    expectedSha: meta.archiveSha256,
    label: `${target}.ffmpegArchive`,
    work,
    extractDir: ffmpegExtractDir,
  });
  const sameArchive =
    ffprobeArchiveUrl === meta.archiveUrl &&
    ffprobeArchiveKind === meta.archiveKind &&
    ffprobeArchiveSha === meta.archiveSha256;
  const ffprobeExtractDir = sameArchive ? ffmpegExtractDir : join(work, "extract-ffprobe");
  if (!sameArchive) {
    await downloadAndExtract({
      url: ffprobeArchiveUrl,
      kind: ffprobeArchiveKind,
      expectedSha: ffprobeArchiveSha,
      label: `${target}.ffprobeArchive`,
      work,
      extractDir: ffprobeExtractDir,
    });
  }

  // manifest 里的成员路径可能带一层顶级目录通配（如 johnvansickle 的 ffmpeg-*-amd64-static/）。
  // 用递归查找 basename 命中，避免把版本号写死进 manifest。
  const wantFf = basename(meta.ffmpegPathInArchive);
  const wantFp = basename(meta.ffprobePathInArchive);
  const foundFf = findFile(ffmpegExtractDir, wantFf);
  const foundFp = findFile(ffprobeExtractDir, wantFp);
  if (!foundFf) die(`归档中未找到 ffmpeg（${wantFf}）`);
  if (!foundFp) die(`归档中未找到 ffprobe（${wantFp}）`);

  copyFileSync(foundFf, ffmpegOut);
  copyFileSync(foundFp, ffprobeOut);
  if (meta.os !== "windows") {
    chmodSync(ffmpegOut, 0o755);
    chmodSync(ffprobeOut, 0o755);
  }

  // 最终文件 SHA256 复校。
  const finalFf = await sha256File(ffmpegOut);
  const finalFp = await sha256File(ffprobeOut);
  if (finalFf !== meta.ffmpegSha256) {
    die(`ffmpeg 最终 SHA256 不匹配：\n  期望 ${meta.ffmpegSha256}\n  实际 ${finalFf}`);
  }
  if (finalFp !== meta.ffprobeSha256) {
    die(`ffprobe 最终 SHA256 不匹配：\n  期望 ${meta.ffprobeSha256}\n  实际 ${finalFp}`);
  }
  console.log("[prepare-media-tools] sidecar SHA256 校验通过。");

  // 能力自检：版本、ffprobe 可执行性和编码器齐全。
  validatePreparedTools(ffmpegOut, ffprobeOut, meta, target);

  // 许可证材料（若指定）：复制到 binaries/licenses/<triple>/；缺失即失败。
  await copyLicenseMaterial(meta, target, [ffmpegExtractDir, ffprobeExtractDir], binDir);

  console.log(`[prepare-media-tools] 完成：\n  ${ffmpegOut}\n  ${ffprobeOut}`);
}

function basename(p) {
  const norm = p.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx >= 0 ? norm.slice(idx + 1) : norm;
}

/** 递归查找目录下 basename 匹配的第一个普通文件。 */
function findFile(dir, name) {
  const stack = [dir];
  while (stack.length > 0) {
    const cur = stack.pop();
    let entries;
    try {
      entries = readdirSync(cur, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      const full = join(cur, e.name);
      if (e.isDirectory()) {
        stack.push(full);
      } else if (!e.isSymbolicLink() && e.isFile() && e.name === name) {
        if (lstatSync(full).isFile()) return full;
      }
    }
  }
  return null;
}

main().catch((e) => die(e instanceof Error ? e.message : String(e)));
