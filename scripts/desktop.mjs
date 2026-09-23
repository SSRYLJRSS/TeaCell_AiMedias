#!/usr/bin/env node
/**
 * 三端统一桌面入口：target triple、HEIF 依赖和媒体 sidecar 的唯一编排处。
 *
 * `check` 只做环境检查；`build` 和 `check --strict-release` 对 sidecar 执行硬门禁。
 * 目标平台差异留在这里和 Rust 能力层，不在 CI 脚本里复制一套命令。
 */
import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const tauri = join(root, "src-tauri");
const manifestPath = join(tauri, "binaries", "manifest.json");
const targets = {
  "x86_64-pc-windows-msvc": { os: "windows", ext: ".exe", heif: "native/heif/x86_64-pc-windows-msvc" },
  "aarch64-apple-darwin": { os: "macos", ext: "", heif: "native/heif/aarch64-apple-darwin" },
  "x86_64-unknown-linux-gnu": { os: "linux", ext: "", heif: "native/heif/x86_64-unknown-linux-gnu" },
};

const sha256 = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const manifest = () => {
  if (!existsSync(manifestPath)) return null;
  try { return JSON.parse(readFileSync(manifestPath, "utf8")); } catch { return null; }
};
const die = (message) => { console.error(`[desktop] ${message}`); process.exit(1); };

function defaultTarget() {
  if (process.platform === "win32" && process.arch === "x64") return "x86_64-pc-windows-msvc";
  if (process.platform === "darwin" && process.arch === "arm64") return "aarch64-apple-darwin";
  if (process.platform === "linux" && process.arch === "x64") return "x86_64-unknown-linux-gnu";
  return null;
}

function parseArgs(args) {
  const [mode, ...rest] = args;
  let target;
  let strict = false;
  const passthrough = [];
  for (let i = 0; i < rest.length; i += 1) {
    if (rest[i] === "--target" && rest[i + 1]) { target = rest[++i]; }
    else if (rest[i] === "--strict-release") strict = true;
    else passthrough.push(rest[i]);
  }
  return { mode, target: target ?? defaultTarget(), strict, passthrough };
}

function checkSidecars(target, meta, strict) {
  const m = manifest()?.targets?.[target];
  const result = [];
  for (const name of ["ffmpeg", "ffprobe"]) {
    const file = join(tauri, "binaries", `${name}-${target}${meta.ext}`);
    const present = existsSync(file) && statSync(file).isFile();
    const problems = [];
    if (strict) {
      if (!m || !present) problems.push(!m ? "manifest 缺少目标" : "文件缺失");
      else {
        const expected = m[`${name}Sha256`];
        if (!/^[a-f0-9]{64}$/i.test(expected ?? "")) problems.push("SHA256 未锁定");
        else if (sha256(file) !== expected) problems.push("SHA256 不匹配");
        if (meta.os !== "windows" && (statSync(file).mode & 0o111) === 0) problems.push("缺少可执行位");
      }
    }
    result.push({ name, file, present, problems });
  }
  const licenseName = m?.licenseFileName ?? "LICENSE.txt";
  const safeLicenseName = typeof licenseName === "string" && licenseName.length > 0 && !/[\\/]/.test(licenseName) && licenseName !== "." && licenseName !== "..";
  const licenseFile = safeLicenseName ? join(tauri, "binaries", "licenses", target, licenseName) : "";
  const licensePresent = existsSync(licenseFile) && statSync(licenseFile).isFile();
  const licenseProblems = [];
  if (strict) {
    if (!safeLicenseName) licenseProblems.push("manifest 许可证文件名无效");
    if (!/^[a-f0-9]{64}$/i.test(m?.licenseSha256 ?? "")) licenseProblems.push("manifest 缺少许可证 SHA256");
    else if (!licensePresent) licenseProblems.push("许可证文件缺失");
    else if (sha256(licenseFile) !== m.licenseSha256) licenseProblems.push("许可证 SHA256 不匹配");
  }
  result.push({ name: "license", file: licenseFile, present: licensePresent, problems: licenseProblems });
  if (strict && result[0].present && result[0].problems.length === 0 && m?.requiredEncoders) {
    const probe = spawnSync(result[0].file, ["-hide_banner", "-encoders"], { encoding: "utf8" });
    const text = `${probe.stdout ?? ""}\n${probe.stderr ?? ""}`;
    const missing = m.requiredEncoders.filter((encoder) => !text.includes(encoder));
    if (probe.status !== 0 || missing.length) result[0].problems.push(`编码器缺失: ${missing.join(",")}`);
  }
  return result;
}

function runTauri(mode, target, passthrough, env) {
  const args = [
    mode === "build" ? "build" : "dev",
    ...(mode === "build" ? ["--target", target, "--config", "src-tauri/tauri.sidecar.conf.json"] : []),
    ...passthrough,
  ];
  const command = process.platform === "win32" ? (process.env.ComSpec || "cmd.exe") : "npm";
  const commandArgs = process.platform === "win32"
    ? ["/d", "/s", "/c", "npm", "run", "tauri", "--", ...args]
    : ["run", "tauri", "--", ...args];
  const result = spawnSync(command, commandArgs, { cwd: root, stdio: "inherit", env });
  if (result.status !== 0) die(`tauri ${mode} 失败（退出码 ${result.status ?? "signal"}）`);
}

function main() {
  const { mode, target: selected, strict, passthrough } = parseArgs(process.argv.slice(2));
  if (!["dev", "build", "check"].includes(mode)) die("用法: npm run desktop:<dev|build|check> -- --target <triple>");
  if (!selected || !targets[selected]) die(`不支持或无法推断 target: ${selected ?? "(none)"}`);
  const meta = targets[selected];
  const effectiveStrict = strict || mode === "build";
  console.log(`[desktop] target=${selected} os=${meta.os} arch=${process.arch}`);

  const env = { ...process.env };
  const heif = join(tauri, ...meta.heif.split("/"));
  const legacy = join(root, "heif-bin");
  if (existsSync(join(heif, "include")) && existsSync(join(heif, "lib"))) env.HEIF_BINARIES_DIR = heif;
  else if (selected === "x86_64-pc-windows-msvc" && existsSync(join(legacy, "include")) && existsSync(join(legacy, "lib"))) env.HEIF_BINARIES_DIR = legacy;
  else delete env.HEIF_BINARIES_DIR;

  const sidecars = checkSidecars(selected, meta, effectiveStrict);
  for (const item of sidecars) console.log(`[desktop] ${item.name}: ${item.present ? item.file : "missing"}${item.problems.length ? ` (${item.problems.join("; ")})` : ""}`);
  const failed = sidecars.filter((item) => item.problems.length);
  if (effectiveStrict && failed.length) die(`发布严格校验失败；请先运行 npm run prepare-media-tools -- --target ${selected}`);
  if (mode === "check") return;
  runTauri(mode, selected, passthrough, env);
}

main();
