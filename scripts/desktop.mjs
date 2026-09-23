#!/usr/bin/env node
/**
 * 三端复核 §7：统一跨平台桌面构建入口（Windows / macOS / Linux）。
 *
 * 职责：
 *  - 按**实际 target triple**决定 HEIF 依赖目录，绝不把 Windows `.lib` 方案注入 macOS/Linux；
 *  - Windows x64 且 native/heif/x86_64-pc-windows-msvc 结构存在时才导出 HEIF_BINARIES_DIR；
 *  - macOS ARM64 / Linux x64 使用各自独立目录（缺失时给明确错误 + 准备命令）；
 *  - 校验并放置 ffmpeg/ffprobe sidecar（src-tauri/binaries/，Tauri externalBin 命名规则）；
 *  - 构建日志输出实际 triple、HEIF 来源、FFmpeg sidecar 路径、bundle 类型。
 *
 * 用法（package.json 脚本转发）：
 *   node scripts/desktop.mjs dev      # tauri dev
 *   node scripts/desktop.mjs build    # tauri build（发布 bundle）
 *   node scripts/desktop.mjs check    # 只做依赖/环境自检，不构建
 *
 * 显式 target：`--target <triple>`；缺省用当前平台默认 triple。
 * CI 与本地文档只调用本入口，不再各自拼 tauri 命令。
 */

import { spawnSync } from "node:child_process";
import { existsSync, statSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "..");
const srcTauri = join(repoRoot, "src-tauri");
const SIDECAR_SENTINEL = "REPLACE_WITH_REAL_SHA256";
const URL_SENTINEL = "REPLACE_WITH_IMMUTABLE_RELEASE_URL";

/** 三个首发目标 triple（Windows x64 / macOS ARM64 / Linux x64）。 */
const KNOWN_TARGETS = {
  "x86_64-pc-windows-msvc": { os: "windows", arch: "x86_64", heifDir: "native/heif/x86_64-pc-windows-msvc", exeExt: ".exe" },
  "aarch64-apple-darwin": { os: "macos", arch: "aarch64", heifDir: "native/heif/aarch64-apple-darwin", exeExt: "" },
  "x86_64-unknown-linux-gnu": { os: "linux", arch: "x86_64", heifDir: "native/heif/x86_64-unknown-linux-gnu", exeExt: "" },
};

/** 当前平台默认 triple（未显式传 --target 时使用）。 */
function defaultTriple() {
  const p = process.platform;
  const a = process.arch;
  if (p === "win32" && a === "x64") return "x86_64-pc-windows-msvc";
  if (p === "darwin" && a === "arm64") return "aarch64-apple-darwin";
  if (p === "linux" && a === "x64") return "x86_64-unknown-linux-gnu";
  return null;
}

function parseArgs(argv) {
  const [mode, ...rest] = argv;
  let target = null;
  let strictRelease = false;
  const passthrough = [];
  for (let i = 0; i < rest.length; i++) {
    if (rest[i] === "--target" && rest[i + 1]) {
      target = rest[i + 1];
      i++;
    } else if (rest[i] === "--strict-release") {
      strictRelease = true;
    } else {
      passthrough.push(rest[i]);
    }
  }
  return { mode, target, strictRelease, passthrough };
}

/** 同步计算文件 SHA256（sidecar 校验用；sidecar 通常几十 MB，一次读入可接受）。 */
function sha256FileSync(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

/** 读取 sidecar manifest（不存在返回 null，让调用方决定是否强制）。 */
function loadSidecarManifest() {
  const p = join(srcTauri, "binaries", "manifest.json");
  if (!existsSync(p)) return null;
  try {
    return JSON.parse(readFileSync(p, "utf8"));
  } catch {
    return null;
  }
}

function die(msg) {
  console.error(`\n[desktop] 错误：${msg}\n`);
  process.exit(1);
}

/** 某目录是否含完整的 libheif 预编译结构（include/ + lib/）。 */
function heifDirComplete(dir) {
  return existsSync(join(dir, "include")) && existsSync(join(dir, "lib"));
}

/** HEIF 依赖检查：仅当目标目录结构存在才导出 env；缺失时给明确准备命令。 */
function resolveHeifEnv(triple, meta) {
  const dir = join(srcTauri, ...meta.heifDir.split("/"));
  if (heifDirComplete(dir)) {
    return { dir, ok: true };
  }
  // 兼容既有 Windows x64 布局：历史依赖放在仓库根 heif-bin/（含 include/ + lib/）。
  // 仅 Windows MSVC 目标允许回退到该目录；其他平台绝不复用。
  if (triple === "x86_64-pc-windows-msvc") {
    const legacy = join(repoRoot, "heif-bin");
    if (heifDirComplete(legacy)) {
      return { dir: legacy, ok: true, legacy: true };
    }
  }
  // 目录不完整：不导出 env，让 build.rs 走各自平台的默认解析（联网下载或系统库）。
  return {
    dir,
    ok: false,
    hint:
      `未找到完整的 HEIF 预编译依赖：${dir}\n` +
      `  期望结构：${meta.heifDir}/include 与 ${meta.heifDir}/lib\n` +
      `  准备方式：将 ${triple} 对应的 libheif include/ 与 lib/ 放入该目录，` +
      `或依赖 build.rs 的平台默认解析（联网/系统库）。`,
  };
}

/** ffmpeg/ffprobe sidecar 校验：Tauri externalBin 打包要求名为 `<name>-<triple><ext>`。
 * strict=true（--strict-release）时额外校验：普通文件 / Unix 可执行位 / SHA256 匹配 manifest /
 * manifest 非哨兵 / 编码器能力 / 许可证材料存在。非 strict 只报告存在性（开发/普通 check 用）。 */
function checkSidecars(triple, meta, strict) {
  const binDir = join(srcTauri, "binaries");
  const manifest = loadSidecarManifest();
  const mtarget = manifest?.targets?.[triple] ?? null;
  const results = [];
  for (const name of ["ffmpeg", "ffprobe"]) {
    const file = join(binDir, `${name}-${triple}${meta.exeExt}`);
    const present = existsSync(file) && statSync(file).isFile();
    const problems = [];
    if (strict) {
      if (!present) {
        problems.push("文件缺失");
      } else {
        const archiveUrl = name === "ffprobe"
          ? (mtarget?.ffprobeArchiveUrl ?? mtarget?.archiveUrl)
          : mtarget?.archiveUrl;
        if (
          typeof archiveUrl !== "string" ||
          archiveUrl === URL_SENTINEL ||
          !/^https?:\/\//i.test(archiveUrl) ||
          /\/latest(?:\/|$)/i.test(archiveUrl)
        ) {
          problems.push(`manifest.${name === "ffprobe" ? "ffprobeArchiveUrl" : "archiveUrl"} 未锁定到不可变版本 URL（禁止 latest）`);
        }
        const archiveSha = name === "ffprobe"
          ? (mtarget?.ffprobeArchiveSha256 ?? mtarget?.archiveSha256)
          : mtarget?.archiveSha256;
        if (!archiveSha || archiveSha === SIDECAR_SENTINEL) {
          problems.push(`manifest.${name === "ffprobe" ? "ffprobeArchiveSha256" : "archiveSha256"} 仍是占位哨兵`);
        }
        // Unix 可执行位
        if (meta.os !== "windows") {
          const mode = statSync(file).mode;
          if ((mode & 0o111) === 0) problems.push("缺少可执行位");
        }
        // SHA256 校验（需 manifest 且非哨兵）
        const expected = mtarget?.[`${name}Sha256`];
        if (!mtarget) {
          problems.push("manifest 缺少该目标条目");
        } else if (!expected || expected === SIDECAR_SENTINEL) {
          problems.push(`manifest.${name}Sha256 仍是占位哨兵（未填真实 SHA256）`);
        } else {
          const got = sha256FileSync(file);
          if (got !== expected) problems.push(`SHA256 不匹配（期望 ${expected} 实际 ${got}）`);
        }
      }
    }
    results.push({ name, file, present, problems });
  }
  // 编码器能力 + 许可证：strict 且 ffmpeg 存在时校验。
  if (strict && results[0].present && results[0].problems.length === 0) {
    const ffmpeg = results[0].file;
    const enc = spawnSync(ffmpeg, ["-hide_banner", "-encoders"], { encoding: "utf8" });
    const required = mtarget?.requiredEncoders ?? [];
    if (enc.status !== 0) {
      results[0].problems.push("ffmpeg -encoders 执行失败");
    } else {
      const encoderText = `${enc.stdout ?? ""}\n${enc.stderr ?? ""}`;
      const missing = required.filter((e) => !encoderText.includes(e));
      if (missing.length > 0) results[0].problems.push(`缺少编码器 ${missing.join(", ")}`);
    }
    if (mtarget?.licensePathInArchive || mtarget?.licenseUrl) {
      const licDir = join(binDir, "licenses", triple);
      const licenseName = mtarget.licenseFileName ||
        (mtarget.licensePathInArchive && mtarget.licensePathInArchive.replace(/\\/g, "/").split("/").pop());
      const licenseFile = licenseName ? join(licDir, licenseName) : null;
      if (!licenseFile || !existsSync(licenseFile) || !statSync(licenseFile).isFile()) {
        results[0].problems.push("缺少许可证材料文件");
      } else if (mtarget.licenseSha256 && mtarget.licenseSha256 !== SIDECAR_SENTINEL) {
        const gotLicense = sha256FileSync(licenseFile);
        if (gotLicense !== mtarget.licenseSha256) {
          results[0].problems.push(`许可证 SHA256 不匹配（期望 ${mtarget.licenseSha256} 实际 ${gotLicense}）`);
        }
      } else if (mtarget.licenseSha256 === SIDECAR_SENTINEL) {
        results[0].problems.push("manifest.licenseSha256 仍是占位哨兵");
      }
    }
  }
  return results;
}

function runTauri(mode, triple, passthrough, env) {
  const args = [];
  if (mode === "dev") {
    args.push("dev");
  } else if (mode === "build") {
    // sidecar externalBin 只在发布构建时经附加配置合入（base tauri.conf.json 不含 externalBin，
    // 以免无 sidecar 时 cargo test/build 因 externalBin 缺文件而失败）。build 前 checkSidecars 已强校验。
    // desktop.mjs 从仓库根调用 npm/tauri；sidecar 配置实际位于 src-tauri/。
    args.push("build", "--target", triple, "--config", "src-tauri/tauri.sidecar.conf.json");
  }
  args.push(...passthrough);

  console.log(`[desktop] 执行：npm run tauri -- ${args.join(" ")}`);
  // Windows 的 npm.cmd 是批处理文件；Node spawnSync 直接执行它在部分 runner 返回 EINVAL。
  // 通过 cmd.exe 显式托管，保证本地与 GitHub Windows runner 的行为一致。
  const npmInvocation = ["run", "tauri", "--", ...args];
  const command = process.platform === "win32" ? (process.env.ComSpec || "cmd.exe") : "npm";
  const commandArgs = process.platform === "win32"
    ? ["/d", "/s", "/c", "npm", ...npmInvocation]
    : npmInvocation;
  // npm run 的脚本参数必须放在 `--` 之后；否则 `build` 会被 npm 当成脚本名而不是 Tauri 子命令。
  const r = spawnSync(command, commandArgs, {
    cwd: repoRoot,
    stdio: "inherit",
    env,
  });
  if (r.status !== 0) {
    die(`tauri ${mode} 失败（退出码 ${r.status ?? "signal"}）`);
  }
}

function main() {
  const { mode, target, strictRelease, passthrough } = parseArgs(process.argv.slice(2));
  if (!["dev", "build", "check"].includes(mode)) {
    die("用法：node scripts/desktop.mjs <dev|build|check> [--target <triple>] [--strict-release] [-- ...]");
  }
  // build 天然按发布严格校验；check 需显式 --strict-release 才严格（普通 check 允许缺 sidecar 只 warn）。
  const strict = mode === "build" || strictRelease;

  const triple = target || defaultTriple();
  if (!triple) {
    die(`无法推断当前平台的 target triple（platform=${process.platform} arch=${process.arch}）。请用 --target 显式指定。`);
  }
  const meta = KNOWN_TARGETS[triple];
  if (!meta) {
    die(
      `不支持的 target：${triple}。首发目标为：\n  ` +
        Object.keys(KNOWN_TARGETS).join("\n  "),
    );
  }

  console.log(`[desktop] 目标 triple : ${triple}（os=${meta.os} arch=${meta.arch}）`);

  // HEIF 依赖：仅在目录完整时导出 env，绝不复用 Windows 目录到别的平台。
  const heif = resolveHeifEnv(triple, meta);
  const env = { ...process.env };
  if (heif.ok) {
    env.HEIF_BINARIES_DIR = heif.dir;
    const tag = heif.legacy ? "（Windows 兼容旧布局 heif-bin/）" : "";
    console.log(`[desktop] HEIF 来源   : ${heif.dir}（已导出 HEIF_BINARIES_DIR）${tag}`);
  } else {
    delete env.HEIF_BINARIES_DIR;
    console.log(`[desktop] HEIF 来源   : 平台默认解析（build.rs），未导出 HEIF_BINARIES_DIR`);
    console.log(`[desktop]              ${heif.hint.replace(/\n/g, "\n[desktop]              ")}`);
  }

  // FFmpeg/FFprobe sidecar：报告存在性/问题。
  //  - 普通 check（非 strict）：缺失只 warn（dev 可用 PATH 兜底）；
  //  - build / --strict-release：文件存在 + 可执行位 + SHA256 匹配 + 编码器 + 许可证全部通过才放行。
  const sidecars = checkSidecars(triple, meta, strict);
  for (const s of sidecars) {
    const status = s.present ? s.file : "缺失（" + s.file + "）";
    const probs = s.problems.length > 0 ? `  [问题: ${s.problems.join("；")}]` : "";
    console.log(`[desktop] sidecar     : ${s.name} → ${status}${probs}`);
  }
  const missing = sidecars.filter((s) => !s.present).map((s) => s.name);
  const failed = sidecars.filter((s) => s.problems.length > 0).map((s) => s.name);
  if (strict && failed.length > 0) {
    die(
      `发布严格校验未通过（sidecar）：${failed.join(", ")}。\n` +
        sidecars
          .filter((s) => s.problems.length > 0)
          .map((s) => `  - ${s.name}: ${s.problems.join("；")}`)
          .join("\n") +
        `\n  准备方式：node scripts/prepare-media-tools.mjs --target ${triple}\n` +
        `  安装包必须自带视频工具（PATH 为空也要能探测/转码），故 build/strict 阶段强制校验。`,
    );
  }

  console.log(`[desktop] bundle 类型 : ${meta.os === "windows" ? "nsis/msi" : meta.os === "macos" ? "app/dmg" : "appimage/deb"}`);

  if (mode === "check") {
    if (strict) {
      console.log("[desktop] strict-release check 完成：sidecar 来源/hash/编码器/许可证均通过。");
    } else {
      console.log("[desktop] check 完成：仅做依赖/环境自检，未触发构建。");
      if (missing.length > 0) {
        console.log(`[desktop] 注意：sidecar 尚缺 ${missing.join(", ")}（dev 可用 PATH 兜底，build 前须补齐）。`);
      }
    }
    return;
  }

  runTauri(mode, triple, passthrough, env);
}

main();
